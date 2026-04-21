use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::channel as channel_writer;
use crate::cli::{ChannelArgs, Cli};
use crate::compile::{self, EnsureMode};
use crate::{devprofile, index, template};

pub async fn run(http: &reqwest::Client, top: &Cli, args: &ChannelArgs) -> Result<()> {
    let templates = index::collect_templates(&top.templates_dir)
        .with_context(|| format!("collecting templates from {}", top.templates_dir.display()))?;

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

    let device_filter = args
        .device
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let ui_filter = args.ui.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let cache_url = Some(args.cache_url.as_str()).filter(|s| !s.is_empty());

    // CacheOnly mode never touches the artifact cache; pass an empty path
    // (the codepath that uses it is gated out before we'd need a real dir).
    let unused_artifact_dir = PathBuf::new();

    let mut bootpros: Vec<PathBuf> = Vec::new();
    for (device, template_path) in &templates {
        if let Some(d) = device_filter
            && d != device
        {
            continue;
        }
        let template_text = tokio::fs::read_to_string(template_path)
            .await
            .with_context(|| format!("reading template {}", template_path.display()))?;

        let mut selections = index::select_rootfs_images(&release, device)
            .with_context(|| format!("selecting rootfs images for {device}"))?;
        if let Some(ui) = ui_filter {
            selections = index::filter_rootfs_images_by_ui(selections, ui)
                .with_context(|| format!("filtering rootfs images for {device}"))?;
        }

        for selection in selections {
            let manifest_content =
                template::render_manifest(&template_text, &release_name, &selection).with_context(
                    || format!("rendering manifest for {}", selection.target_name()),
                )?;
            let bootpro = compile::ensure_bootpro(
                http,
                &bootpro_tool_version,
                &release_name,
                cache_url,
                &manifest_content,
                &selection,
                Path::new(&unused_artifact_dir),
                &args.bootpro_cache_dir,
                EnsureMode::CacheOnly,
            )
            .await
            .with_context(|| format!("ensuring bootpro for {}", selection.target_name()))?;
            bootpros.push(bootpro);
        }
    }

    if bootpros.is_empty() {
        anyhow::bail!("no bootpros selected; refusing to write empty channel");
    }

    let mut devpros: Vec<PathBuf> = Vec::new();
    for (device_id, source_path) in devprofile::collect_sources(&args.devprofiles_dir)
        .with_context(|| {
            format!(
                "collecting devprofiles from {}",
                args.devprofiles_dir.display()
            )
        })?
    {
        let devpro = args.devpro_build_dir.join(format!("{device_id}.devpro"));
        devprofile::compile(&source_path, &devpro)
            .await
            .with_context(|| format!("compiling devprofile {}", source_path.display()))?;
        tracing::info!(device_id = %device_id, path = %devpro.display(), "compiled devprofile");
        devpros.push(devpro);
    }

    let mut records = Vec::with_capacity(devpros.len() + bootpros.len());
    records.extend(devpros);
    records.extend(bootpros);

    channel_writer::write_indexed_channel(&records, &args.output)
        .await
        .with_context(|| format!("writing channel to {}", args.output.display()))?;

    Ok(())
}
