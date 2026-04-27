use anyhow::{Context, Result};
use std::collections::BTreeSet;

use crate::cli::{Cli, ListArgs};
use crate::index;

#[derive(Debug, serde::Serialize)]
struct Entry {
    device: String,
    ui: String,
}

pub async fn run(http: &reqwest::Client, top: &Cli, args: &ListArgs) -> Result<()> {
    let templates = index::collect_templates(&top.templates_dir)
        .with_context(|| format!("collecting templates from {}", top.templates_dir.display()))?;

    let release = index::fetch_release(http, &top.index_url, &top.release)
        .await
        .with_context(|| format!("fetching release {:?} from {}", top.release, top.index_url))?;

    let device_filter = args
        .device
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let ui_filter = args.ui.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for device in templates.keys() {
        if let Some(d) = device_filter
            && d != device
        {
            continue;
        }
        let mut selections = index::select_rootfs_images(&release, device)
            .with_context(|| format!("selecting rootfs images for {device}"))?;
        if let Some(ui) = ui_filter {
            selections = index::filter_rootfs_images_by_ui(selections, ui)
                .with_context(|| format!("filtering rootfs images for {device}"))?;
        }
        for sel in selections {
            seen.insert((device.clone(), sel.ui_name));
        }
    }

    let entries: Vec<Entry> = seen
        .into_iter()
        .map(|(device, ui)| Entry { device, ui })
        .collect();

    println!("{}", serde_json::to_string(&entries)?);
    Ok(())
}
