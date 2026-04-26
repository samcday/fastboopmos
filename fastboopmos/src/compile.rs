use anyhow::{Context, Result};
use fastboop_bootpro::{BootProfileOptimizeOptions, compile_manifest_yaml_with_optimize};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::artifact;
use crate::index::RootfsSelection;

const HTTP_CACHE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn scope_hash(manifest_content: &str, bootpro_tool_version: &str) -> String {
    let payload = format!("{bootpro_tool_version}\n{manifest_content}");
    let digest = Sha256::digest(payload.as_bytes());
    hex::encode(digest)[..24].to_string()
}

/// GET the bootpro from a public HTTP cache if present. Returns Ok(true) on
/// 200 (file written to `destination`), Ok(false) on 404 or any transient
/// failure (so the caller falls through to a local compile).
async fn try_fetch_remote_bootpro(
    http: &reqwest::Client,
    cache_url: &str,
    release_name: &str,
    filename: &str,
    destination: &Path,
) -> Result<bool> {
    let base = cache_url.trim_end_matches('/');
    let url = format!("{base}/{release_name}/bootpro/{filename}");
    tracing::debug!(url = %url, "trying HTTP cache");

    let resp = match http.get(&url).timeout(HTTP_CACHE_TIMEOUT).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "HTTP cache request failed, falling back to compile");
            return Ok(false);
        }
    };

    match resp.status() {
        reqwest::StatusCode::OK => {}
        reqwest::StatusCode::NOT_FOUND => return Ok(false),
        s => {
            tracing::warn!(url = %url, status = %s, "HTTP cache returned unexpected status, falling back to compile");
            return Ok(false);
        }
    }

    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut temp_filename = destination
        .file_name()
        .context("destination has no file name")?
        .to_os_string();
    temp_filename.push(".tmp");
    let temp_path = destination.with_file_name(temp_filename);

    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(&temp_path)
        .await
        .with_context(|| format!("creating {}", temp_path.display()))?;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(url = %url, error = %e, "HTTP cache stream broke mid-download, falling back to compile");
                drop(file);
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Ok(false);
            }
        };
        file.write_all(&chunk)
            .await
            .with_context(|| format!("writing to {}", temp_path.display()))?;
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    tokio::fs::rename(&temp_path, destination)
        .await
        .with_context(|| {
            format!(
                "renaming {} -> {}",
                temp_path.display(),
                destination.display()
            )
        })?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub async fn ensure_bootpro(
    http: &reqwest::Client,
    bootpro_tool_version: &str,
    release_name: &str,
    cache_url: Option<&str>,
    manifest_content: &str,
    selection: &RootfsSelection,
    artifact_cache_dir: &Path,
    bootpro_cache_dir: &Path,
) -> Result<PathBuf> {
    let scope = scope_hash(manifest_content, bootpro_tool_version);
    let filename = format!("{}-{}.bootpro", selection.image_sha512, scope);
    let output_path = bootpro_cache_dir.join(&filename);

    if tokio::fs::try_exists(&output_path).await? {
        tracing::info!(target = %selection.target_name(), path = %output_path.display(), "bootpro local cache hit");
        return Ok(output_path);
    }

    if let Some(base) = cache_url
        && try_fetch_remote_bootpro(http, base, release_name, &filename, &output_path).await?
    {
        tracing::info!(target = %selection.target_name(), path = %output_path.display(), "bootpro HTTP cache hit");
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

    let (compiled, optimized) = compile_manifest_yaml_with_optimize(
        manifest_content.as_bytes(),
        BootProfileOptimizeOptions {
            local_artifacts: vec![local_artifact],
            materialized_cache_dir: None,
        },
    )
    .await
    .with_context(|| format!("compiling bootpro for {}", selection.target_name()))?;

    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut bytes = compiled.bytes;
    bytes.extend_from_slice(optimized.bytes.as_slice());
    tokio::fs::write(&output_path, bytes)
        .await
        .with_context(|| format!("writing compiled bootpro to {}", output_path.display()))?;
    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_hash_matches_python() {
        // sha256("fastboop 0.0.1\nhello")[:24]
        // Cross-checked with: printf 'fastboop 0.0.1\nhello' | sha256sum | cut -c1-24
        assert_eq!(
            scope_hash("hello", "fastboop 0.0.1"),
            "fcaa4cab284ce907a8be3ac8"
        );
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
