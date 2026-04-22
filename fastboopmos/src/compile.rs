use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::artifact;
use crate::index::RootfsSelection;

pub async fn fastboop_version(fastboop: &Path) -> Result<String> {
    let output = Command::new(fastboop)
        .arg("--version")
        .output()
        .await
        .with_context(|| format!("running {} --version", fastboop.display()))?;
    if !output.status.success() {
        bail!(
            "failed to determine fastboop version from {}: {}",
            fastboop.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8(output.stdout).context("fastboop --version stdout is not UTF-8")?;
    let first_line = stdout
        .trim()
        .lines()
        .next()
        .context("fastboop --version returned empty output")?
        .trim()
        .to_string();
    if first_line.is_empty() {
        bail!("fastboop --version returned empty output");
    }
    Ok(first_line)
}

pub fn scope_hash(manifest_content: &str, fastboop_ver: &str) -> String {
    let payload = format!("{fastboop_ver}\n{manifest_content}");
    let digest = Sha256::digest(payload.as_bytes());
    hex::encode(digest)[..24].to_string()
}

#[allow(clippy::too_many_arguments)]
pub async fn ensure_bootpro(
    http: &reqwest::Client,
    fastboop: &Path,
    fastboop_ver: &str,
    manifest_content: &str,
    selection: &RootfsSelection,
    artifact_cache_dir: &Path,
    bootpro_cache_dir: &Path,
) -> Result<PathBuf> {
    let scope = scope_hash(manifest_content, fastboop_ver);
    let output_path = bootpro_cache_dir.join(format!(
        "{}-{}.bootpro",
        selection.image_sha512, scope
    ));

    if tokio::fs::try_exists(&output_path).await? {
        tracing::info!(target = %selection.target_name(), path = %output_path.display(), "bootpro cache hit");
        return Ok(output_path);
    }

    tracing::info!(target = %selection.target_name(), "bootpro cache miss, compiling");
    let local_artifact = artifact::ensure_cached(
        http,
        &selection.image_url,
        &selection.image_sha512,
        selection.image_size,
        artifact_cache_dir,
    )
    .await?;

    let temp_dir = tempfile::Builder::new()
        .prefix("bootpro-build-")
        .tempdir()
        .context("creating temp dir for bootpro build")?;
    let manifest_path = temp_dir.path().join("manifest.yaml");
    let compiled_path = temp_dir.path().join("out.bootpro");

    tokio::fs::write(&manifest_path, manifest_content)
        .await
        .with_context(|| format!("writing manifest to {}", manifest_path.display()))?;

    let status = Command::new(fastboop)
        .arg("bootprofile")
        .arg("create")
        .arg(&manifest_path)
        .arg("-o")
        .arg(&compiled_path)
        .arg("--optimize")
        .arg("--local-artifact")
        .arg(&local_artifact)
        .status()
        .await
        .with_context(|| format!("invoking {} bootprofile create", fastboop.display()))?;
    if !status.success() {
        bail!(
            "fastboop bootprofile create failed with status {} for {}",
            status,
            selection.target_name()
        );
    }

    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::rename(&compiled_path, &output_path)
        .await
        .with_context(|| {
            format!(
                "moving compiled bootpro {} -> {}",
                compiled_path.display(),
                output_path.display()
            )
        })?;
    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_hash_matches_python() {
        // sha256("fastboop 0.0.1\nhello")[:24]
        // Cross-checked with: printf 'fastboop 0.0.1\nhello' | sha256sum | cut -c1-24
        assert_eq!(scope_hash("hello", "fastboop 0.0.1"), "fcaa4cab284ce907a8be3ac8");
    }

    #[test]
    fn scope_hash_stable() {
        let a = scope_hash("manifest-body", "fastboop 0.0.1-rc.15");
        let b = scope_hash("manifest-body", "fastboop 0.0.1-rc.15");
        assert_eq!(a, b);
        assert_eq!(a.len(), 24);
    }

    #[test]
    fn scope_hash_different_version() {
        let a = scope_hash("m", "v1");
        let b = scope_hash("m", "v2");
        assert_ne!(a, b);
    }
}
