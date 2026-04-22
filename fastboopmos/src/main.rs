use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use fastboopmos::cli::Args;
use fastboopmos::{channel, compile, index, template};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    let args = Args::parse();
    let templates_dir = args.templates_dir.clone();
    let artifact_cache_dir = args.artifact_cache_dir.clone();
    let bootpro_cache_dir = args.bootpro_cache_dir.clone();
    let output = args.output.clone();

    let http = reqwest::Client::builder()
        .build()
        .context("failed to build reqwest client")?;

    let templates = index::collect_templates(&templates_dir)
        .with_context(|| format!("collecting templates from {}", templates_dir.display()))?;

    let selected_templates: Vec<(String, PathBuf)> = match args.only_device.as_deref() {
        Some(device) => {
            let device = device.trim();
            let path = templates
                .get(device)
                .with_context(|| format!("template not found for device {device:?}"))?
                .clone();
            vec![(device.to_string(), path)]
        }
        None => templates.into_iter().collect(),
    };

    let release = index::fetch_release(&http, &args.index_url, &args.release)
        .await
        .with_context(|| format!("fetching release {:?} from {}", args.release, args.index_url))?;
    let release_name = release
        .get("name")
        .and_then(|v| v.as_str())
        .context("release is missing a name")?
        .to_string();

    let fastboop_ver = compile::fastboop_version(&args.fastboop)
        .await
        .with_context(|| format!("determining fastboop version from {}", args.fastboop.display()))?;
    tracing::info!(version = %fastboop_ver, "using fastboop");

    let mut selected_bootpros: Vec<PathBuf> = Vec::new();
    for (pmos_device, template_path) in selected_templates {
        let template_text = tokio::fs::read_to_string(&template_path)
            .await
            .with_context(|| format!("reading template {}", template_path.display()))?;
        let selections = index::select_rootfs_images(&release, &pmos_device)
            .with_context(|| format!("selecting rootfs images for {pmos_device}"))?;

        for selection in selections {
            let manifest_content = template::render_manifest(&template_text, &release_name, &selection)
                .with_context(|| format!("rendering manifest for {}", selection.target_name()))?;
            let bootpro = compile::ensure_bootpro(
                &http,
                &args.fastboop,
                &fastboop_ver,
                &manifest_content,
                &selection,
                &artifact_cache_dir,
                &bootpro_cache_dir,
            )
            .await
            .with_context(|| format!("ensuring bootpro for {}", selection.target_name()))?;
            selected_bootpros.push(bootpro);
        }
    }

    channel::write_indexed_channel(&args.fastboop, &selected_bootpros, &output)
        .await
        .with_context(|| format!("writing channel to {}", output.display()))?;

    Ok(())
}
