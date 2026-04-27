use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RootfsSelection {
    pub pmos_device: String,
    pub ui_name: String,
    pub variant: Option<String>,
    pub image_name: String,
    pub image_url: String,
    pub image_sha512: String,
    pub image_size: u64,
    pub timestamp: String,
}

impl RootfsSelection {
    pub fn target_name(&self) -> String {
        match &self.variant {
            None => self.pmos_device.clone(),
            Some(v) => format!("{}-{}", self.pmos_device, v),
        }
    }
}

pub fn collect_templates(templates_dir: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut templates = BTreeMap::new();
    let read = std::fs::read_dir(templates_dir)
        .with_context(|| format!("reading directory {}", templates_dir.display()))?;
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
            .context("template has no stem")?;
        templates.insert(stem, path);
    }
    if templates.is_empty() {
        bail!("no device templates found in {}", templates_dir.display());
    }
    Ok(templates)
}

pub async fn fetch_release(
    http: &reqwest::Client,
    index_url: &str,
    release_name: &str,
) -> Result<Value> {
    let payload: Value = http
        .get(index_url)
        .send()
        .await
        .context("GET index.json")?
        .error_for_status()
        .context("index.json responded with non-success")?
        .json()
        .await
        .context("parsing index.json")?;

    let releases = payload
        .get("releases")
        .and_then(|v| v.as_array())
        .context("index.json is missing releases")?;

    for release in releases {
        if let Some(name) = release.get("name").and_then(|v| v.as_str())
            && name == release_name
        {
            return Ok(release.clone());
        }
    }
    bail!("release {release_name:?} not found in {index_url}");
}

/// Mirrors `rootfs_variant` in `scripts/build_channel.py:207`.
///
/// Returns:
/// - `None` if the image is not a rootfs (wrong suffix, or -boot/-bootpart)
/// - `Some(String::new())` for the bare `{pmos_device}.img.xz` image (empty variant)
/// - `Some("<variant>")` for `{pmos_device}-<variant>.img.xz`
pub fn rootfs_variant(image_name: &str, pmos_device: &str) -> Option<String> {
    if !image_name.ends_with(".img.xz") {
        return None;
    }
    if image_name.ends_with("-boot.img.xz") || image_name.ends_with("-bootpart.img.xz") {
        return None;
    }
    let bare_suffix = format!("-{pmos_device}.img.xz");
    if image_name.ends_with(&bare_suffix) {
        return Some(String::new());
    }
    let marker = format!("-{pmos_device}-");
    let idx = image_name.rfind(&marker)?;
    let variant = &image_name[idx + marker.len()..image_name.len() - ".img.xz".len()];
    if variant.is_empty() {
        None
    } else {
        Some(variant.to_string())
    }
}

pub fn select_rootfs_images(release: &Value, pmos_device: &str) -> Result<Vec<RootfsSelection>> {
    let release_devices = release
        .get("devices")
        .and_then(|v| v.as_array())
        .context("release is missing devices")?;

    let device_entry = release_devices
        .iter()
        .find(|item| item.get("name").and_then(|v| v.as_str()) == Some(pmos_device))
        .with_context(|| format!("device {pmos_device:?} not found in release"))?;

    let interfaces = device_entry
        .get("interfaces")
        .and_then(|v| v.as_array())
        .with_context(|| format!("device {pmos_device:?} has no interfaces list"))?;

    // Preserve insertion order of the (ui_name, variant_key) groupings to match Python's
    // dict iteration order (Python 3.7+ dicts preserve insertion order).
    let mut order: Vec<(String, String)> = Vec::new();
    let mut grouped: BTreeMap<(String, String), Vec<&Value>> = BTreeMap::new();

    for interface in interfaces {
        let ui_name = match interface.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        let images = match interface.get("images").and_then(|v| v.as_array()) {
            Some(i) => i,
            None => continue,
        };

        for image in images {
            let image_name = match image.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };
            let image_url = image.get("url").and_then(|v| v.as_str());
            let timestamp = image.get("timestamp").and_then(|v| v.as_str());
            if image_url.is_none() || timestamp.is_none() {
                continue;
            }
            image
                .get("sha512")
                .and_then(|v| v.as_str())
                .filter(|s| s.len() == 128)
                .with_context(|| format!("image {image_name:?} is missing sha512"))?;
            image
                .get("size")
                .and_then(|v| v.as_u64())
                .filter(|s| *s > 0)
                .with_context(|| format!("image {image_name:?} has invalid size"))?;

            let variant_key = match rootfs_variant(image_name, pmos_device) {
                Some(v) => v,
                None => continue,
            };

            let key = (ui_name.clone(), variant_key);
            if !grouped.contains_key(&key) {
                order.push(key.clone());
            }
            grouped.entry(key).or_default().push(image);
        }
    }

    let mut selections: Vec<RootfsSelection> = Vec::new();
    for key in order {
        let options = grouped.get(&key).unwrap();
        let latest = options
            .iter()
            .max_by(|a, b| {
                let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
                let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
                ta.cmp(tb)
            })
            .unwrap();

        let image_name = latest
            .get("name")
            .and_then(|v| v.as_str())
            .context("image missing name")?
            .to_string();
        let image_url = latest
            .get("url")
            .and_then(|v| v.as_str())
            .context("image missing url")?
            .to_string();
        let image_sha512 = latest
            .get("sha512")
            .and_then(|v| v.as_str())
            .filter(|s| s.len() == 128)
            .with_context(|| format!("image {image_name:?} is missing sha512"))?
            .to_string();
        let image_size = latest
            .get("size")
            .and_then(|v| v.as_u64())
            .filter(|s| *s > 0)
            .with_context(|| format!("image {image_name:?} has invalid size"))?;
        let timestamp = latest
            .get("timestamp")
            .and_then(|v| v.as_str())
            .with_context(|| format!("image {image_name:?} is missing timestamp"))?
            .to_string();

        let (ui_name, variant_key) = key;
        let variant = if variant_key.is_empty() {
            None
        } else {
            Some(variant_key)
        };
        selections.push(RootfsSelection {
            pmos_device: pmos_device.to_string(),
            ui_name,
            variant,
            image_name,
            image_url,
            image_sha512,
            image_size,
            timestamp,
        });
    }

    selections.sort_by(|a, b| {
        (
            a.pmos_device.as_str(),
            a.ui_name.as_str(),
            a.variant.as_deref().unwrap_or(""),
        )
            .cmp(&(
                b.pmos_device.as_str(),
                b.ui_name.as_str(),
                b.variant.as_deref().unwrap_or(""),
            ))
    });

    if selections.is_empty() {
        bail!("no usable rootfs images found for {pmos_device:?}");
    }
    Ok(selections)
}

pub fn filter_rootfs_images_by_ui(
    selections: Vec<RootfsSelection>,
    requested: &str,
) -> Result<Vec<RootfsSelection>> {
    let requested = requested.trim();
    if requested.is_empty() {
        bail!("--ui must not be empty");
    }

    let filtered: Vec<RootfsSelection> = selections
        .into_iter()
        .filter(|selection| selection.ui_name == requested)
        .collect();
    if filtered.is_empty() {
        bail!("no usable rootfs images found for ui {requested:?}");
    }
    Ok(filtered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(ui_name: &str, variant: Option<&str>) -> RootfsSelection {
        RootfsSelection {
            pmos_device: "oneplus-fajita".into(),
            ui_name: ui_name.into(),
            variant: variant.map(str::to_string),
            image_name: "x".into(),
            image_url: "y".into(),
            image_sha512: "z".into(),
            image_size: 1,
            timestamp: "t".into(),
        }
    }

    #[test]
    fn variant_bare() {
        assert_eq!(
            rootfs_variant(
                "20260101-0000-postmarketOS-v24.06-plasma-mobile-5-oneplus-fajita.img.xz",
                "oneplus-fajita"
            ),
            Some(String::new())
        );
    }

    #[test]
    fn variant_named() {
        assert_eq!(
            rootfs_variant(
                "20260101-0000-postmarketOS-v24.06-plasma-mobile-5-oneplus-fajita-factory.img.xz",
                "oneplus-fajita"
            ),
            Some("factory".to_string())
        );
    }

    #[test]
    fn variant_boot_excluded() {
        assert_eq!(
            rootfs_variant(
                "20260101-0000-postmarketOS-v24.06-plasma-mobile-5-oneplus-fajita-boot.img.xz",
                "oneplus-fajita"
            ),
            None
        );
        assert_eq!(
            rootfs_variant("something-oneplus-fajita-bootpart.img.xz", "oneplus-fajita"),
            None
        );
    }

    #[test]
    fn variant_not_rootfs() {
        assert_eq!(rootfs_variant("something.zip", "oneplus-fajita"), None);
        assert_eq!(
            rootfs_variant("no-device-here.img.xz", "oneplus-fajita"),
            None
        );
    }

    #[test]
    fn variant_trailing_marker_only() {
        // name ends with marker then ".img.xz" — extracted variant is empty => None
        assert_eq!(
            rootfs_variant("xx-oneplus-fajita-.img.xz", "oneplus-fajita"),
            None
        );
    }

    #[test]
    fn target_name_bare() {
        let sel = selection("Plasma Mobile", None);
        assert_eq!(sel.target_name(), "oneplus-fajita");
    }

    #[test]
    fn target_name_variant() {
        let sel = selection("Plasma Mobile", Some("factory"));
        assert_eq!(sel.target_name(), "oneplus-fajita-factory");
    }

    #[test]
    fn filters_rootfs_images_by_ui_name() {
        let selections = vec![selection("phosh", None), selection("gnome-mobile", None)];
        let filtered = filter_rootfs_images_by_ui(selections, "phosh").unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].ui_name, "phosh");
    }

    #[test]
    fn filters_rootfs_images_keeps_all_hw_variants_for_ui() {
        // Both ebbg and tianma share ui_name="phosh" — filtering on the UI
        // keeps both, and the per-selection compile loop handles each panel.
        let selections = vec![
            selection("phosh", Some("ebbg")),
            selection("phosh", Some("tianma")),
            selection("gnome-mobile", Some("ebbg")),
        ];
        let filtered = filter_rootfs_images_by_ui(selections, "phosh").unwrap();
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn rejects_missing_ui_filter() {
        let err = filter_rootfs_images_by_ui(vec![selection("phosh", None)], "plasma").unwrap_err();
        assert!(format!("{err}").contains("no usable rootfs images"));
    }
}
