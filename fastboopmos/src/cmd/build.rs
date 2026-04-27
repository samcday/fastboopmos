use anyhow::{Context, Result};

use crate::cli::{BuildArgs, Cli};
use crate::compile::{self, EnsureMode};
use crate::{index, template};

pub async fn run(http: &reqwest::Client, top: &Cli, args: &BuildArgs) -> Result<()> {
    let templates = index::collect_templates(&top.templates_dir)
        .with_context(|| format!("collecting templates from {}", top.templates_dir.display()))?;

    let device = args.device.trim();
    let template_path = templates
        .get(device)
        .with_context(|| format!("template not found for device {device:?}"))?
        .clone();
    let template_text = tokio::fs::read_to_string(&template_path)
        .await
        .with_context(|| format!("reading template {}", template_path.display()))?;

    let release = index::fetch_release(http, &top.index_url, &top.release)
        .await
        .with_context(|| format!("fetching release {:?} from {}", top.release, top.index_url))?;
    let release_name = release
        .get("name")
        .and_then(|v| v.as_str())
        .context("release is missing a name")?
        .to_string();

    let bootpro_tool_version = env!("FASTBOOP_BOOTPRO_VERSION").to_string();
    tracing::info!(version = %bootpro_tool_version, "using bootpro compiler");

    let mut selections = index::select_rootfs_images(&release, device)
        .with_context(|| format!("selecting rootfs images for {device}"))?;
    if let Some(ui) = args.ui.as_deref()
        && !ui.trim().is_empty()
    {
        selections = index::filter_rootfs_images_by_ui(selections, ui)
            .with_context(|| format!("filtering rootfs images for {device}"))?;
    }

    let cache_url = Some(args.cache_url.as_str()).filter(|s| !s.is_empty());

    for selection in selections {
        let manifest_content = template::render_manifest(&template_text, &release_name, &selection)
            .with_context(|| format!("rendering manifest for {}", selection.target_name()))?;
        compile::ensure_bootpro(
            http,
            &bootpro_tool_version,
            &release_name,
            cache_url,
            &manifest_content,
            &selection,
            &args.artifact_cache_dir,
            &args.bootpro_cache_dir,
            EnsureMode::Compile,
        )
        .await
        .with_context(|| format!("ensuring bootpro for {}", selection.target_name()))?;
    }

    Ok(())
}
