use std::{
    collections::HashMap,
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::{self, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
};

const CHUNK_SIZE: usize = 1024 * 1024;
const USER_AGENT: &str = "fastboopmos-frontdoor/0.1";
const GH_API_VERSION: &str = "2022-11-28";
const PER_PAGE: u32 = 100;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Config {
    port: u16,
    cache_dir: PathBuf,
    min_freespace_pct: f64,
    request_timeout_secs: u64,
    github_owner: String,
    github_repo: String,
    github_token: String,
    edge_artifact_id: String,
    asset_name: String,
    sha256_asset_name: String,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_u64(name: &str, default: u64) -> u64 {
    let raw = env_or(name, "");
    if raw.is_empty() {
        return default;
    }
    raw.parse()
        .unwrap_or_else(|_| panic!("{name} must be an integer"))
}

impl Config {
    fn from_env() -> Self {
        Self {
            port: env_u64("PORT", 8080) as u16,
            cache_dir: PathBuf::from(env_or("CACHE_DIR", "/cache")),
            min_freespace_pct: {
                let raw = env_or("MIN_FREESPACE_PCT", "10");
                raw.parse::<f64>()
                    .unwrap_or_else(|_| panic!("MIN_FREESPACE_PCT must be a number"))
            },
            request_timeout_secs: env_u64("REQUEST_TIMEOUT_SECONDS", 300),
            github_owner: env_or("GITHUB_OWNER", "samcday"),
            github_repo: env_or("GITHUB_REPO", "fastboopmos"),
            github_token: env_or("GITHUB_TOKEN", ""),
            edge_artifact_id: env_or("EDGE_CHANNEL_ARTIFACT_ID", "").trim().to_string(),
            asset_name: env_or("ASSET_NAME", "edge.channel"),
            sha256_asset_name: env_or("SHA256_ASSET_NAME", "edge.channel.sha256"),
        }
    }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct AppState {
    config: Config,
    http: reqwest::Client,
    gha_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    release_lock: Mutex<()>,
}

impl AppState {
    fn new(config: Config) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            USER_AGENT.parse().unwrap(),
        );
        headers.insert("x-github-api-version", GH_API_VERSION.parse().unwrap());
        headers.insert(
            reqwest::header::ACCEPT,
            "application/vnd.github+json".parse().unwrap(),
        );
        if !config.github_token.is_empty() {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", config.github_token).parse().unwrap(),
            );
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(config.request_timeout_secs))
            .build()
            .expect("failed to build HTTP client");

        Self {
            config,
            http,
            gha_locks: Mutex::new(HashMap::new()),
            release_lock: Mutex::new(()),
        }
    }

    fn gha_cache_dir(&self) -> PathBuf {
        self.config.cache_dir.join("gha")
    }

    fn release_cache_dir(&self) -> PathBuf {
        self.config.cache_dir.join("release")
    }

    async fn gha_lock_for(&self, key: &str) -> Arc<Mutex<()>> {
        let mut locks = self.gha_locks.lock().await;
        locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn gha_lock_remove(&self, key: &str) {
        let mut locks = self.gha_locks.lock().await;
        locks.remove(key);
    }
}

type SharedState = Arc<AppState>;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

struct AppError(StatusCode, String);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, format!("{}\n", self.1)).into_response()
    }
}

impl AppError {
    fn not_found() -> Self {
        Self(StatusCode::NOT_FOUND, "not found".into())
    }

    fn bad_gateway(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_GATEWAY, msg.into())
    }
}

// ---------------------------------------------------------------------------
// GitHub helpers
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ArtifactListResponse {
    artifacts: Vec<Artifact>,
}

#[derive(serde::Deserialize)]
struct Artifact {
    expired: Option<bool>,
    archive_download_url: Option<String>,
}

async fn resolve_single_artifact(state: &AppState, run_id: &str) -> Result<String, AppError> {
    let mut page = 1u32;
    let mut artifacts = Vec::new();

    loop {
        let url = format!(
            "https://api.github.com/repos/{}/{}/actions/runs/{}/artifacts?per_page={}&page={}",
            state.config.github_owner, state.config.github_repo, run_id, PER_PAGE, page,
        );
        let resp = state
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::bad_gateway(format!("GitHub API request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::not_found());
        }
        if !resp.status().is_success() {
            return Err(AppError::bad_gateway(format!(
                "GitHub API request failed: {}",
                resp.status()
            )));
        }

        let payload: ArtifactListResponse = resp
            .json()
            .await
            .map_err(|e| AppError::bad_gateway(format!("invalid JSON: {e}")))?;

        let count = payload.artifacts.len();
        for a in payload.artifacts {
            if a.expired != Some(true) {
                artifacts.push(a);
            }
        }
        if count < PER_PAGE as usize {
            break;
        }
        page += 1;
    }

    if artifacts.is_empty() {
        return Err(AppError(
            StatusCode::NOT_FOUND,
            format!("no active artifacts found for run {run_id}"),
        ));
    }
    if artifacts.len() != 1 {
        return Err(AppError(
            StatusCode::CONFLICT,
            format!(
                "run {run_id} has {} active artifacts; expected exactly 1",
                artifacts.len()
            ),
        ));
    }

    artifacts
        .remove(0)
        .archive_download_url
        .ok_or_else(|| AppError::bad_gateway("artifact is missing archive_download_url"))
}

async fn download_archive(state: &AppState, url: &str, dest: &Path) -> Result<(), AppError> {
    use tokio::io::AsyncWriteExt;

    let resp = state
        .http
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::bad_gateway(format!("download failed: {e}")))?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AppError::not_found());
    }
    if !resp.status().is_success() {
        return Err(AppError::bad_gateway(format!(
            "download failed: {}",
            resp.status()
        )));
    }

    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("create: {e}")))?;

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| AppError::bad_gateway(format!("download stream failed: {e}")))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("write: {e}")))?;
    }

    file.flush()
        .await
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("flush: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Hex encoding (avoid pulling in another dep)
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Temp dir cleanup guard
// ---------------------------------------------------------------------------

struct TmpDirGuard {
    path: PathBuf,
    defused: bool,
}

impl TmpDirGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            defused: false,
        }
    }

    fn defuse(&mut self) {
        self.defused = true;
    }
}

impl Drop for TmpDirGuard {
    fn drop(&mut self) {
        if !self.defused {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

// ---------------------------------------------------------------------------
// GHA proxy: extract single file from zip, cache by run ID
// ---------------------------------------------------------------------------

struct GhaCacheEntry {
    blob_path: PathBuf,
    content_type: String,
    etag: String,
    size: u64,
}

fn extract_single_file_from_zip(
    zip_path: &Path,
    blob_path: &Path,
) -> Result<(String, String), AppError> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("open zip: {e}")))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| AppError::bad_gateway("artifact archive is not a valid zip file"))?;

    let file_indices: Vec<usize> = (0..archive.len())
        .filter(|&i| {
            archive
                .by_index(i)
                .map(|e| {
                    !e.is_dir() && !e.name().ends_with(".sha256") && !e.name().ends_with(".sha256sum")
                })
                .unwrap_or(false)
        })
        .collect();

    if file_indices.len() != 1 {
        let all_names: Vec<String> = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
            .collect();
        return Err(AppError(
            StatusCode::CONFLICT,
            format!(
                "artifact archive contains {} non-checksum files; expected exactly 1 (files: {})",
                file_indices.len(),
                all_names.join(", "),
            ),
        ));
    }

    let mut entry = archive.by_index(file_indices[0]).unwrap();
    let filename = entry.name().to_string();

    let mut hasher = Sha256::new();
    let mut out = std::fs::File::create(blob_path)
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("create blob: {e}")))?;

    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = entry
            .read(&mut buf)
            .map_err(|e| AppError::bad_gateway(format!("read zip: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut out, &buf[..n])
            .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("write: {e}")))?;
    }

    let digest = hex_encode(&hasher.finalize());
    let etag = format!("\"sha256-{digest}\"");
    let content_type = mime_guess::from_path(&filename)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string();

    Ok((content_type, etag))
}

async fn gha_load_cache_entry(cache_dir: &Path, key: &str) -> Option<GhaCacheEntry> {
    let blob_path = cache_dir.join(format!("{key}.blob"));
    let meta_path = cache_dir.join(format!("{key}.json"));

    let meta_bytes = fs::read(&meta_path).await.ok()?;
    let meta: serde_json::Value = serde_json::from_slice(&meta_bytes).ok()?;
    let size = fs::metadata(&blob_path).await.ok()?.len();

    Some(GhaCacheEntry {
        blob_path,
        content_type: meta["content_type"].as_str()?.to_string(),
        etag: meta["etag"].as_str()?.to_string(),
        size,
    })
}

async fn gha_materialize(state: &AppState, run_id: &str) -> Result<GhaCacheEntry, AppError> {
    let cache_dir = state.gha_cache_dir();
    fs::create_dir_all(&cache_dir).await.ok();

    let key = {
        let input = format!(
            "{}/{}:{}",
            state.config.github_owner, state.config.github_repo, run_id
        );
        hex_encode(&Sha256::digest(input.as_bytes()))
    };

    // fast path
    if let Some(entry) = gha_load_cache_entry(&cache_dir, &key).await {
        return Ok(entry);
    }

    let lock = state.gha_lock_for(&key).await;
    let _guard = lock.lock().await;

    // re-check after lock
    if let Some(entry) = gha_load_cache_entry(&cache_dir, &key).await {
        state.gha_lock_remove(&key).await;
        return Ok(entry);
    }

    let archive_url = resolve_single_artifact(state, run_id).await?;

    let tmp_dir = cache_dir.join(format!(".tmp-{key}"));
    fs::create_dir_all(&tmp_dir).await.ok();
    let mut tmp_guard = TmpDirGuard::new(tmp_dir.clone());

    let zip_path = tmp_dir.join("artifact.zip");
    let blob_tmp = tmp_dir.join("blob.tmp");

    download_archive(state, &archive_url, &zip_path).await?;

    let zp = zip_path.clone();
    let bp = blob_tmp.clone();
    let (content_type, etag) =
        tokio::task::spawn_blocking(move || extract_single_file_from_zip(&zp, &bp))
            .await
            .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))??;

    let blob_path = cache_dir.join(format!("{key}.blob"));
    let meta_path = cache_dir.join(format!("{key}.json"));

    let size = fs::metadata(&blob_tmp)
        .await
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("stat: {e}")))?
        .len();

    let meta = serde_json::json!({
        "size": size,
        "content_type": content_type,
        "etag": etag,
    });

    fs::rename(&blob_tmp, &blob_path)
        .await
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("rename blob: {e}")))?;
    fs::write(&meta_path, serde_json::to_string(&meta).unwrap())
        .await
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("write meta: {e}")))?;

    tmp_guard.defuse();
    let _ = fs::remove_dir_all(&tmp_dir).await;

    state.gha_lock_remove(&key).await;
    enforce_disk_freespace(state).await;

    gha_load_cache_entry(&cache_dir, &key)
        .await
        .ok_or_else(|| {
            AppError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load cached artifact after extraction".into(),
            )
        })
}

// ---------------------------------------------------------------------------
// Disk-pressure-aware cache eviction
// ---------------------------------------------------------------------------

fn freespace_pct(path: &Path) -> Option<f64> {
    use std::ffi::CString;
    let c_path = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    unsafe {
        let mut buf: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut buf) != 0 {
            return None;
        }
        let total = buf.f_blocks as f64;
        if total == 0.0 {
            return None;
        }
        let free = buf.f_bfree as f64;
        Some((free / total) * 100.0)
    }
}

async fn enforce_disk_freespace(state: &AppState) {
    if state.config.min_freespace_pct <= 0.0 {
        return;
    }

    let cache_dir = &state.config.cache_dir;

    // check before doing any work
    if let Some(pct) = freespace_pct(cache_dir) {
        if pct >= state.config.min_freespace_pct {
            return;
        }
        tracing::info!(
            "free space {pct:.1}% below threshold {:.1}%, evicting old cache entries",
            state.config.min_freespace_pct,
        );
    } else {
        return;
    }

    // collect all evictable .blob files across gha/ and release/ subdirs
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for subdir in [state.gha_cache_dir(), state.release_cache_dir()] {
        let mut dir = match fs::read_dir(&subdir).await {
            Ok(d) => d,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("blob") {
                if let Ok(meta) = fs::metadata(&path).await {
                    let mtime =
                        meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    entries.push((mtime, path));
                }
            }
        }
    }

    entries.sort_by_key(|(mtime, _)| *mtime);

    while !entries.is_empty() {
        if let Some(pct) = freespace_pct(cache_dir) {
            if pct >= state.config.min_freespace_pct {
                break;
            }
        } else {
            break;
        }

        let (_, path) = entries.remove(0);
        let meta_path = path.with_extension("json");
        tracing::info!("evicting {}", path.display());
        let _ = fs::remove_file(&path).await;
        let _ = fs::remove_file(&meta_path).await;
    }
}

// ---------------------------------------------------------------------------
// Release: serve edge channel assets from a specific artifact
// ---------------------------------------------------------------------------

fn extract_named_file_from_zip(
    zip_path: &Path,
    name: &str,
    dest: &Path,
) -> Result<(), AppError> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("open zip: {e}")))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| AppError::bad_gateway("artifact archive is not a valid zip file"))?;

    let mut found = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).unwrap();
        if !entry.is_dir() {
            let entry_name = Path::new(entry.name())
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if entry_name == name {
                found = Some(i);
                break;
            }
        }
    }

    let idx = found
        .ok_or_else(|| AppError::bad_gateway(format!("artifact archive is missing {name}")))?;

    let mut entry = archive.by_index(idx).unwrap();
    let mut out = std::fs::File::create(dest)
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("create: {e}")))?;

    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = entry
            .read(&mut buf)
            .map_err(|e| AppError::bad_gateway(format!("read zip: {e}")))?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut out, &buf[..n])
            .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("write: {e}")))?;
    }
    Ok(())
}

async fn release_materialize(state: &AppState) -> Result<(), AppError> {
    let artifact_id = &state.config.edge_artifact_id;
    let release_dir = state.release_cache_dir();
    let target_dir = release_dir.join(artifact_id);
    let channel_path = target_dir.join(&state.config.asset_name);
    let sha256_path = target_dir.join(&state.config.sha256_asset_name);

    if fs::try_exists(&channel_path).await.unwrap_or(false)
        && fs::try_exists(&sha256_path).await.unwrap_or(false)
    {
        return Ok(());
    }

    let _guard = state.release_lock.lock().await;

    if fs::try_exists(&channel_path).await.unwrap_or(false)
        && fs::try_exists(&sha256_path).await.unwrap_or(false)
    {
        return Ok(());
    }

    fs::create_dir_all(&target_dir).await.ok();

    let tmp_dir = release_dir.join(format!(".tmp-{artifact_id}"));
    fs::create_dir_all(&tmp_dir).await.ok();
    let mut tmp_guard = TmpDirGuard::new(tmp_dir.clone());

    let zip_path = tmp_dir.join("artifact.zip");
    let url = format!(
        "https://api.github.com/repos/{}/{}/actions/artifacts/{}/zip",
        state.config.github_owner, state.config.github_repo, artifact_id,
    );

    download_archive(state, &url, &zip_path).await?;

    let zp = zip_path.clone();
    let asset = state.config.asset_name.clone();
    let sha = state.config.sha256_asset_name.clone();
    let ch_tmp = tmp_dir.join(&asset);
    let sha_tmp = tmp_dir.join(&sha);

    tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        extract_named_file_from_zip(&zp, &asset, &ch_tmp)?;
        extract_named_file_from_zip(&zp, &sha, &sha_tmp)?;
        Ok(())
    })
    .await
    .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))??;

    fs::rename(tmp_dir.join(&state.config.asset_name), &channel_path)
        .await
        .map_err(|e| {
            AppError(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("rename channel asset: {e}"),
            )
        })?;
    fs::rename(
        tmp_dir.join(&state.config.sha256_asset_name),
        &sha256_path,
    )
    .await
    .map_err(|e| {
        AppError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("rename sha256 asset: {e}"),
        )
    })?;

    tmp_guard.defuse();
    let _ = fs::remove_dir_all(&tmp_dir).await;

    enforce_disk_freespace(state).await;

    Ok(())
}

// ---------------------------------------------------------------------------
// CORS helpers for release endpoints
// ---------------------------------------------------------------------------

const ALLOWED_ORIGINS_EXACT: &[&str] =
    &["https://www.fastboop.win", "https://bleeding.fastboop.win"];
const ALLOWED_LOCALHOST_HOSTS: &[&str] = &["localhost", "127.0.0.1"];

fn is_allowed_origin(origin: &str) -> bool {
    if ALLOWED_ORIGINS_EXACT.contains(&origin) {
        return true;
    }
    if let Ok(url) = url::Url::parse(origin) {
        if let Some(host) = url.host_str() {
            if ALLOWED_LOCALHOST_HOSTS.contains(&host) {
                return true;
            }
        }
    }
    false
}

fn release_cors_headers(origin: &str) -> Vec<(&'static str, String)> {
    if origin.is_empty() || !is_allowed_origin(origin) {
        return Vec::new();
    }
    vec![
        ("access-control-allow-origin", origin.to_string()),
        ("vary", "Origin".to_string()),
        (
            "access-control-allow-methods",
            "GET, HEAD, OPTIONS".to_string(),
        ),
        (
            "access-control-allow-headers",
            "Content-Type, Range".to_string(),
        ),
        (
            "access-control-expose-headers",
            "Content-Length, Content-Range, ETag, Accept-Ranges".to_string(),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Range parsing
// ---------------------------------------------------------------------------

fn parse_single_byte_range(header: &str, size: u64) -> Result<(u64, u64), ()> {
    let spec = header.strip_prefix("bytes=").ok_or(())?;
    let spec = spec.trim();
    if spec.is_empty() || spec.contains(',') {
        return Err(());
    }
    let (start_raw, end_raw) = spec.split_once('-').ok_or(())?;
    let start_raw = start_raw.trim();
    let end_raw = end_raw.trim();

    if start_raw.is_empty() {
        if end_raw.is_empty() {
            return Err(());
        }
        let suffix_len: u64 = end_raw.parse().map_err(|_| ())?;
        if suffix_len == 0 {
            return Err(());
        }
        if suffix_len >= size {
            return Ok((0, size - 1));
        }
        return Ok((size - suffix_len, size - 1));
    }

    let start: u64 = start_raw.parse().map_err(|_| ())?;
    if start >= size {
        return Err(());
    }

    if end_raw.is_empty() {
        return Ok((start, size - 1));
    }

    let end: u64 = end_raw.parse().map_err(|_| ())?;
    if end < start {
        return Err(());
    }
    Ok((start, end.min(size - 1)))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn healthz() -> &'static str {
    "ok"
}

async fn gha_handler(
    State(state): State<SharedState>,
    extract::Path(run_id): extract::Path<String>,
    req: axum::http::Request<Body>,
) -> Result<Response, AppError> {
    if run_id.is_empty() || !run_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::not_found());
    }

    let entry = gha_materialize(&state, &run_id).await?;
    let size = entry.size;

    // Range request
    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let (status, start, end) = if let Some(ref range_str) = range_header {
        match parse_single_byte_range(range_str, size) {
            Ok((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e),
            Err(()) => {
                return Ok(Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header("cache-control", "public, max-age=31536000, immutable")
                    .header("accept-ranges", "bytes")
                    .header("content-range", format!("bytes */{size}"))
                    .header("content-length", "0")
                    .header("access-control-allow-origin", "*")
                    .header("access-control-allow-methods", "GET, HEAD, OPTIONS")
                    .header("access-control-allow-headers", "Content-Type, Range")
                    .header(
                        "access-control-expose-headers",
                        "Content-Length, Content-Range, ETag, Accept-Ranges",
                    )
                    .body(Body::empty())
                    .unwrap());
            }
        }
    } else {
        (StatusCode::OK, 0u64, size.saturating_sub(1))
    };

    let length = end - start + 1;

    let mut builder = Response::builder()
        .status(status)
        .header("cache-control", "public, max-age=31536000, immutable")
        .header("accept-ranges", "bytes")
        .header("content-type", &entry.content_type)
        .header("content-length", length.to_string())
        .header("etag", &entry.etag)
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-methods", "GET, HEAD, OPTIONS")
        .header("access-control-allow-headers", "Content-Type, Range")
        .header(
            "access-control-expose-headers",
            "Content-Length, Content-Range, ETag, Accept-Ranges",
        );

    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header("content-range", format!("bytes {start}-{end}/{size}"));
    }

    if *req.method() == Method::HEAD {
        return Ok(builder.body(Body::empty()).unwrap());
    }

    let mut file = tokio::fs::File::open(&entry.blob_path)
        .await
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("open: {e}")))?;

    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("seek: {e}")))?;
    }

    let stream = tokio_util::io::ReaderStream::new(file.take(length));
    Ok(builder.body(Body::from_stream(stream)).unwrap())
}

async fn gha_options_handler() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-methods", "GET, HEAD, OPTIONS")
        .header("access-control-allow-headers", "Content-Type, Range")
        .header(
            "access-control-expose-headers",
            "Content-Length, Content-Range, ETag, Accept-Ranges",
        )
        .header("access-control-max-age", "86400")
        .header("content-length", "0")
        .body(Body::empty())
        .unwrap()
}

async fn release_asset_handler(
    State(state): State<SharedState>,
    req: axum::http::Request<Body>,
) -> Result<Response, AppError> {
    let path = req.uri().path();
    let asset_name = if path == "/edge.channel" {
        &state.config.asset_name
    } else if path == "/edge.channel.sha256" {
        &state.config.sha256_asset_name
    } else {
        return Err(AppError::not_found());
    };

    release_materialize(&state).await?;

    let asset_path = state
        .release_cache_dir()
        .join(&state.config.edge_artifact_id)
        .join(asset_name);

    let size = fs::metadata(&asset_path)
        .await
        .map_err(|_| AppError::not_found())?
        .len();

    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let cors = release_cors_headers(origin);

    // Range request
    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let (status, start, end) = if let Some(ref range_str) = range_header {
        match parse_single_byte_range(range_str, size) {
            Ok((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e),
            Err(()) => {
                let mut b = Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header("cache-control", "no-store")
                    .header("accept-ranges", "bytes")
                    .header("content-range", format!("bytes */{size}"))
                    .header("content-length", "0");
                for (k, v) in &cors {
                    b = b.header(*k, v.as_str());
                }
                return Ok(b.body(Body::empty()).unwrap());
            }
        }
    } else {
        (StatusCode::OK, 0u64, size.saturating_sub(1))
    };

    let length = end - start + 1;
    let content_type = mime_guess::from_path(&asset_path)
        .first_raw()
        .unwrap_or("application/octet-stream");

    let mut builder = Response::builder()
        .status(status)
        .header("cache-control", "no-store")
        .header("accept-ranges", "bytes")
        .header("content-type", content_type)
        .header("content-length", length.to_string());

    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header("content-range", format!("bytes {start}-{end}/{size}"));
    }
    for (k, v) in &cors {
        builder = builder.header(*k, v.as_str());
    }

    if *req.method() == Method::HEAD {
        return Ok(builder.body(Body::empty()).unwrap());
    }

    let mut file = tokio::fs::File::open(&asset_path)
        .await
        .map_err(|_| AppError::not_found())?;

    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("seek: {e}")))?;
    }

    let stream = tokio_util::io::ReaderStream::new(file.take(length));
    Ok(builder.body(Body::from_stream(stream)).unwrap())
}

async fn release_options_handler(req: axum::http::Request<Body>) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let cors = release_cors_headers(origin);

    let mut b = Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("access-control-max-age", "86400")
        .header("content-length", "0");
    for (k, v) in &cors {
        b = b.header(*k, v.as_str());
    }
    b.body(Body::empty()).unwrap()
}

async fn release_live_handler(State(state): State<SharedState>) -> String {
    format!("artifact_id={}\n", state.config.edge_artifact_id)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    let config = Config::from_env();

    if config.github_owner.is_empty() || config.github_repo.is_empty() {
        panic!("GITHUB_OWNER and GITHUB_REPO must be configured");
    }
    if config.edge_artifact_id.is_empty()
        || !config.edge_artifact_id.chars().all(|c| c.is_ascii_digit())
    {
        panic!("EDGE_CHANNEL_ARTIFACT_ID must be a numeric GitHub artifact id");
    }

    let addr = format!("0.0.0.0:{}", config.port);
    tracing::info!("listening on {addr}");

    let state: SharedState = Arc::new(AppState::new(config));

    let app = Router::new()
        .route("/healthz", get(healthz))
        // GHA proxy
        .route(
            "/gha/{run_id}",
            get(gha_handler)
                .head(gha_handler)
                .options(gha_options_handler),
        )
        // Release channel
        .route(
            "/edge.channel",
            get(release_asset_handler)
                .head(release_asset_handler)
                .options(release_options_handler),
        )
        .route(
            "/edge.channel.sha256",
            get(release_asset_handler)
                .head(release_asset_handler)
                .options(release_options_handler),
        )
        .route(
            "/__fastboopmos/live",
            get(release_live_handler).options(release_options_handler),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
        _ = sigint.recv() => tracing::info!("received SIGINT, shutting down"),
    }
}
