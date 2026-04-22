use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use sha2::{Digest, Sha512};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use url::Url;

/// Mirrors Python `Path(url.path).suffixes` joined — e.g. `img.img.xz` -> `.img.xz`.
/// Leading dots in the filename are skipped so that hidden files (e.g. `.bashrc`)
/// report no suffixes.
fn joined_suffixes(filename: &str) -> String {
    let trimmed = filename.trim_start_matches('.');
    match trimmed.find('.') {
        Some(idx) => trimmed[idx..].to_string(),
        None => String::new(),
    }
}

fn cached_artifact_path(image_url: &str, image_sha512: &str, cache_dir: &Path) -> Result<PathBuf> {
    let parsed = Url::parse(image_url).with_context(|| format!("parsing URL {image_url}"))?;
    let path = parsed.path();
    let file_name = path.rsplit('/').next().unwrap_or("");
    let suffix = joined_suffixes(file_name);
    let filename = if suffix.is_empty() {
        image_sha512.to_string()
    } else {
        format!("{image_sha512}{suffix}")
    };
    Ok(cache_dir.join(filename))
}

pub async fn ensure_cached(
    http: &reqwest::Client,
    image_url: &str,
    image_sha512: &str,
    image_size: u64,
    cache_dir: &Path,
) -> Result<PathBuf> {
    let output_path = cached_artifact_path(image_url, image_sha512, cache_dir)?;

    if tokio::fs::try_exists(&output_path).await? {
        if verify_existing(&output_path, image_sha512, image_size).await? {
            return Ok(output_path);
        }
        tokio::fs::remove_file(&output_path)
            .await
            .with_context(|| format!("removing stale cache file {}", output_path.display()))?;
    }

    tokio::fs::create_dir_all(cache_dir)
        .await
        .with_context(|| format!("creating cache dir {}", cache_dir.display()))?;

    let mut temp_filename = output_path
        .file_name()
        .context("cache path has no file name")?
        .to_os_string();
    temp_filename.push(".tmp");
    let temp_path = output_path.with_file_name(temp_filename);

    tracing::info!(url = image_url, "downloading artifact");
    let response = http
        .get(image_url)
        .send()
        .await
        .with_context(|| format!("GET {image_url}"))?
        .error_for_status()
        .with_context(|| format!("GET {image_url} responded with non-success"))?;

    let mut stream = response.bytes_stream();
    let mut hasher = Sha512::new();
    let mut size: u64 = 0;

    {
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .with_context(|| format!("creating temp file {}", temp_path.display()))?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading body chunk")?;
            hasher.update(&chunk);
            size += chunk.len() as u64;
            file.write_all(&chunk)
                .await
                .with_context(|| format!("writing to {}", temp_path.display()))?;
        }
        file.flush().await?;
        file.sync_all().await?;
    }

    if size != image_size {
        let _ = tokio::fs::remove_file(&temp_path).await;
        bail!("downloaded artifact size mismatch for {image_url}: expected {image_size}, got {size}");
    }
    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(image_sha512) {
        let _ = tokio::fs::remove_file(&temp_path).await;
        bail!("downloaded artifact digest mismatch for {image_url}");
    }

    tokio::fs::rename(&temp_path, &output_path)
        .await
        .with_context(|| {
            format!(
                "renaming {} -> {}",
                temp_path.display(),
                output_path.display()
            )
        })?;
    Ok(output_path)
}

async fn verify_existing(path: &Path, image_sha512: &str, image_size: u64) -> Result<bool> {
    let meta = tokio::fs::metadata(path).await?;
    if meta.len() != image_size {
        return Ok(false);
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha512::new();
    let mut buf = vec![0u8; 1024 * 1024];
    use tokio::io::AsyncReadExt;
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = hex::encode(hasher.finalize());
    Ok(actual.eq_ignore_ascii_case(image_sha512))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_suffixes_single() {
        assert_eq!(joined_suffixes("foo.xz"), ".xz");
    }

    #[test]
    fn joined_suffixes_double() {
        assert_eq!(joined_suffixes("foo.img.xz"), ".img.xz");
    }

    #[test]
    fn joined_suffixes_none() {
        assert_eq!(joined_suffixes("foo"), "");
    }

    #[test]
    fn joined_suffixes_hidden() {
        assert_eq!(joined_suffixes(".bashrc"), "");
        assert_eq!(joined_suffixes(".bashrc.bak"), ".bak");
    }

    #[test]
    fn cached_artifact_path_pmos_style() {
        // Matches Python's ''.join(Path(url.path).suffixes) — every segment after
        // the first dot in the basename is concatenated, not just the last couple.
        // Verified: python3 -c "from pathlib import Path; print(''.join(Path('…').suffixes))"
        let path = cached_artifact_path(
            "https://images.postmarketos.org/bpo/edge/plasma/20260101-0000-postmarketOS-v24.06-plasma-mobile-5-oneplus-fajita.img.xz",
            "deadbeef",
            Path::new("/tmp/cache"),
        )
        .unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/cache/deadbeef.06-plasma-mobile-5-oneplus-fajita.img.xz")
        );
    }

    #[test]
    fn cached_artifact_path_simple_basename() {
        let path = cached_artifact_path(
            "https://example.invalid/image.img.xz",
            "abc",
            Path::new("/c"),
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/c/abc.img.xz"));
    }

    #[test]
    fn cached_artifact_path_no_extension() {
        let path = cached_artifact_path(
            "https://example.invalid/image",
            "abc",
            Path::new("/c"),
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/c/abc"));
    }
}
