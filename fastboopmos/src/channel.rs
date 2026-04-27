use anyhow::{Context, Result, anyhow, bail};
use fastboop_core::index_channel_bytes;
use std::path::{Path, PathBuf};

pub async fn write_indexed_channel(bootpros: &[PathBuf], output: &Path) -> Result<()> {
    if bootpros.is_empty() {
        bail!("no bootprofiles selected for channel");
    }
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut raw_channel = Vec::new();
    for bootpro in bootpros {
        let bytes = tokio::fs::read(bootpro)
            .await
            .with_context(|| format!("reading {}", bootpro.display()))?;
        raw_channel.extend_from_slice(bytes.as_slice());
    }

    let indexed = index_channel_bytes(raw_channel.as_slice()).map_err(|err| anyhow!("{err}"))?;
    tokio::fs::write(output, indexed)
        .await
        .with_context(|| format!("writing indexed channel to {}", output.display()))?;
    Ok(())
}
