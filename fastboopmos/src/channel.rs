use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub async fn write_indexed_channel(
    fastboop: &Path,
    bootpros: &[PathBuf],
    output: &Path,
) -> Result<()> {
    if bootpros.is_empty() {
        bail!("no bootprofiles selected for channel");
    }
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let temp_dir = tempfile::Builder::new()
        .prefix("channel-build-")
        .tempdir()
        .context("creating temp dir for channel build")?;
    let raw_channel = temp_dir.path().join("raw.channel");
    {
        let mut channel = tokio::fs::File::create(&raw_channel)
            .await
            .with_context(|| format!("creating {}", raw_channel.display()))?;
        for bootpro in bootpros {
            let bytes = tokio::fs::read(bootpro)
                .await
                .with_context(|| format!("reading {}", bootpro.display()))?;
            channel.write_all(&bytes).await?;
        }
        channel.flush().await?;
        channel.sync_all().await?;
    }

    let status = Command::new(fastboop)
        .arg("channel")
        .arg("index")
        .arg(&raw_channel)
        .arg("-o")
        .arg(output)
        .status()
        .await
        .with_context(|| format!("invoking {} channel index", fastboop.display()))?;
    if !status.success() {
        bail!("fastboop channel index failed with status {}", status);
    }
    Ok(())
}
