use anyhow::{Context, Result};
use fastboop_core::{DeviceProfile, encode_dev_profile};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

pub fn collect_sources(dir: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut sources = BTreeMap::new();
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(sources),
        Err(e) => {
            return Err(anyhow::Error::from(e))
                .with_context(|| format!("reading directory {}", dir.display()));
        }
    };
    for entry in read {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if file_name.starts_with('.') {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .context("devprofile source has no stem")?;
        sources.insert(stem, path);
    }
    Ok(sources)
}

pub async fn compile(source: &Path, output: &Path) -> Result<()> {
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    let yaml = tokio::fs::read(source)
        .await
        .with_context(|| format!("reading devprofile {}", source.display()))?;
    let profile: DeviceProfile = serde_yaml::from_slice(&yaml)
        .with_context(|| format!("parsing devprofile {}", source.display()))?;
    let encoded = encode_dev_profile(&profile)
        .with_context(|| format!("encoding devprofile {}", profile.id))?;
    tokio::fs::write(output, encoded)
        .await
        .with_context(|| format!("writing devprofile to {}", output.display()))?;
    Ok(())
}
