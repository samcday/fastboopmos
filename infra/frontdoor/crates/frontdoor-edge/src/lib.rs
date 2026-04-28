use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Once, OnceLock};
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use async_lock::Mutex;
use frontdoor_core::cache_key::{gha_cache_key, hex_encode};
use frontdoor_core::content_type::content_type_for_ext;
use frontdoor_core::cors::is_allowed_release_origin;
use frontdoor_core::id::is_numeric_id;
use frontdoor_core::range::parse_single_byte_range;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, error, info};
use wstd::http::{Body, BodyExt, Client, Request, Response, StatusCode};

const CHUNK_SIZE: usize = 1024 * 1024;
const USER_AGENT: &str = "fastboopmos-frontdoor/0.1";
const GH_API_VERSION: &str = "2022-11-28";
const PER_PAGE: u32 = 100;
const GHA_EDGE_CHANNEL_ARTIFACT_NAME: &str = "edge-channel";
const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;

static TRACING_INIT: Once = Once::new();
static CONFIG: OnceLock<Config> = OnceLock::new();
static GHA_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
static RELEASE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug)]
struct Config {
    cache_dir: String,
    github_owner: String,
    github_repo: String,
    github_token: String,
    edge_artifact_id: String,
    asset_name: String,
    sha256_asset_name: String,
    max_download_bytes: u64,
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

#[derive(Debug)]
struct GhaCacheEntry {
    blob_path: PathBuf,
    content_type: String,
    etag: String,
    size: u64,
}

#[derive(Deserialize, Serialize)]
struct GhaMeta {
    content_type: String,
    etag: String,
}

#[derive(Deserialize)]
struct ArtifactListResponse {
    artifacts: Vec<Artifact>,
}

#[derive(Deserialize)]
struct Artifact {
    name: String,
    expired: Option<bool>,
    archive_download_url: Option<String>,
}

impl Config {
    fn from_env() -> Self {
        let edge_artifact_id = env_or("EDGE_CHANNEL_ARTIFACT_ID", "").trim().to_string();
        if !is_numeric_id(&edge_artifact_id) {
            panic!("EDGE_CHANNEL_ARTIFACT_ID must be a numeric GitHub artifact id");
        }

        let github_owner = env_or("GITHUB_OWNER", "samcday");
        let github_repo = env_or("GITHUB_REPO", "fastboopmos");
        if github_owner.is_empty() || github_repo.is_empty() {
            panic!("GITHUB_OWNER and GITHUB_REPO must be configured");
        }

        let github_token = std::env::var("GITHUB_TOKEN")
            .or_else(|_| std::env::var("token"))
            .unwrap_or_default();

        Self {
            cache_dir: env_or("CACHE_DIR", "/cache"),
            github_owner,
            github_repo,
            github_token,
            edge_artifact_id,
            asset_name: env_or("ASSET_NAME", "edge.channel"),
            sha256_asset_name: env_or("SHA256_ASSET_NAME", "edge.channel.sha256"),
            max_download_bytes: env_u64("MAX_DOWNLOAD_BYTES", DEFAULT_MAX_DOWNLOAD_BYTES),
        }
    }

    fn gha_cache_dir(&self) -> PathBuf {
        PathBuf::from(&self.cache_dir).join("gha")
    }

    fn release_cache_dir(&self) -> PathBuf {
        PathBuf::from(&self.cache_dir).join("release")
    }
}

impl AppError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not found")
    }

    fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

struct TmpDirGuard {
    path: PathBuf,
    active: bool,
}

impl TmpDirGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    fn defuse(&mut self) {
        self.active = false;
    }
}

impl Drop for TmpDirGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[wstd::http_server]
async fn main(request: Request<Body>) -> Result<Response<Body>, wstd::http::Error> {
    ensure_tracing();
    let cfg = config();
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();
    let range = request
        .headers()
        .get("range")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let origin = request
        .headers()
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();

    info!(method = %method, path = %path, "incoming request");

    let response = if path == "/healthz" {
        text_response(StatusCode::OK, "ok")
    } else if let Some(run_id) = parse_gha_path(&path) {
        match method.as_str() {
            "GET" | "HEAD" => gha_handler(cfg, run_id, method == "HEAD", range).await,
            "OPTIONS" => gha_options_response(),
            _ => method_not_allowed_response(),
        }
    } else if path == "/edge.channel" || path == "/edge.channel.sha256" {
        match method.as_str() {
            "GET" | "HEAD" => {
                release_asset_handler(cfg, &path, method == "HEAD", range, &origin).await
            }
            "OPTIONS" => release_options_response(&origin),
            _ => method_not_allowed_response(),
        }
    } else if path == "/__fastboopmos/live" {
        match method.as_str() {
            "GET" | "HEAD" => text_response(
                StatusCode::OK,
                &format!("artifact_id={}\n", cfg.edge_artifact_id),
            ),
            "OPTIONS" => release_options_response(&origin),
            _ => method_not_allowed_response(),
        }
    } else {
        text_response(StatusCode::NOT_FOUND, "not found\n")
    };

    Ok(response)
}

fn ensure_tracing() {
    TRACING_INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_target(false)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".parse().expect("valid env filter")),
            )
            .init();
    });
}

fn config() -> &'static Config {
    CONFIG.get_or_init(|| {
        let cfg = Config::from_env();
        info!(
            cache_dir = %cfg.cache_dir,
            github_owner = %cfg.github_owner,
            github_repo = %cfg.github_repo,
            edge_artifact_id = %cfg.edge_artifact_id,
            max_download_bytes = cfg.max_download_bytes,
            "config loaded"
        );
        cfg
    })
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

fn parse_gha_path(path: &str) -> Option<&str> {
    let run_id = path.strip_prefix("/gha/")?;
    if run_id.contains('/') {
        return None;
    }
    Some(run_id)
}

async fn gha_handler(
    cfg: &Config,
    run_id: &str,
    head: bool,
    range: Option<String>,
) -> Response<Body> {
    if !is_numeric_id(run_id) {
        return text_response(StatusCode::NOT_FOUND, "not found\n");
    }

    let entry = match gha_materialize(cfg, run_id).await {
        Ok(entry) => entry,
        Err(err) => return error_response(err),
    };

    serve_file(FileResponse {
        path: &entry.blob_path,
        size: entry.size,
        content_type: &entry.content_type,
        cache_control: "public, max-age=31536000, immutable",
        etag: Some(&entry.etag),
        cors: CorsPolicy::GhaWildcard,
        head,
        range_header: range.as_deref(),
    })
}

async fn release_asset_handler(
    cfg: &Config,
    path: &str,
    head: bool,
    range: Option<String>,
    origin: &str,
) -> Response<Body> {
    let asset_name = if path == "/edge.channel" {
        &cfg.asset_name
    } else if path == "/edge.channel.sha256" {
        &cfg.sha256_asset_name
    } else {
        return text_response(StatusCode::NOT_FOUND, "not found\n");
    };

    if let Err(err) = release_materialize(cfg).await {
        return error_response(err);
    }

    let asset_path = cfg
        .release_cache_dir()
        .join(&cfg.edge_artifact_id)
        .join(asset_name);
    let size = match fs::metadata(&asset_path) {
        Ok(meta) => meta.len(),
        Err(_) => return text_response(StatusCode::NOT_FOUND, "not found\n"),
    };
    let content_type = content_type_for_path(&asset_path).to_string();

    serve_file(FileResponse {
        path: &asset_path,
        size,
        content_type: &content_type,
        cache_control: "no-store",
        etag: None,
        cors: CorsPolicy::Release(origin.to_string()),
        head,
        range_header: range.as_deref(),
    })
}

async fn gha_materialize(cfg: &Config, run_id: &str) -> Result<GhaCacheEntry, AppError> {
    let cache_dir = cfg.gha_cache_dir();
    fs::create_dir_all(&cache_dir).map_err(io_500)?;

    let key = hex_encode(&gha_cache_key(&cfg.github_owner, &cfg.github_repo, run_id));
    if let Some(entry) = gha_load_cache_entry(&cache_dir, &key) {
        return Ok(entry);
    }

    let lock = gha_lock_for(&key).await;
    let result = {
        let _guard = lock.lock().await;
        async {
            if let Some(entry) = gha_load_cache_entry(&cache_dir, &key) {
                Ok(entry)
            } else {
                let archive_url = resolve_edge_channel_artifact(cfg, run_id).await?;
                let tmp_dir = create_temp_dir(&cache_dir, &format!("gha-{key}"))?;
                let mut tmp_guard = TmpDirGuard::new(tmp_dir.clone());
                let zip_path = tmp_dir.join("artifact.zip");
                let blob_tmp = tmp_dir.join("blob.tmp");
                let meta_tmp = tmp_dir.join("meta.json");

                download_archive(cfg, &archive_url, &zip_path).await?;
                let (content_type, etag) = extract_single_file_from_zip(&zip_path, &blob_tmp)?;
                let blob_path = cache_dir.join(format!("{key}.blob"));
                let meta_path = cache_dir.join(format!("{key}.json"));
                let meta = GhaMeta { content_type, etag };
                let meta_bytes =
                    serde_json::to_vec(&meta).expect("GHA cache metadata should serialize");

                fs::write(&meta_tmp, &meta_bytes).map_err(io_500)?;
                fs::rename(&blob_tmp, &blob_path).map_err(io_500)?;
                fs::rename(&meta_tmp, &meta_path).map_err(io_500)?;

                tmp_guard.defuse();
                let _ = fs::remove_dir_all(&tmp_dir);

                gha_load_cache_entry(&cache_dir, &key).ok_or_else(|| {
                    AppError::internal("failed to load cached artifact after extraction")
                })
            }
        }
        .await
    };
    gha_lock_remove_if_idle(&key, &lock).await;
    result
}

async fn gha_lock_for(key: &str) -> Arc<Mutex<()>> {
    let locks = GHA_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = locks.lock().await;
    guard
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

async fn gha_lock_remove_if_idle(key: &str, lock: &Arc<Mutex<()>>) {
    let locks = GHA_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = locks.lock().await;
    if guard
        .get(key)
        .map(|current| Arc::ptr_eq(current, lock) && Arc::strong_count(current) == 2)
        .unwrap_or(false)
    {
        guard.remove(key);
    }
}

fn gha_load_cache_entry(cache_dir: &Path, key: &str) -> Option<GhaCacheEntry> {
    let blob_path = cache_dir.join(format!("{key}.blob"));
    let meta_path = cache_dir.join(format!("{key}.json"));
    let meta: GhaMeta = serde_json::from_slice(&fs::read(meta_path).ok()?).ok()?;
    let size = fs::metadata(&blob_path).ok()?.len();

    Some(GhaCacheEntry {
        blob_path,
        content_type: meta.content_type,
        etag: meta.etag,
        size,
    })
}

async fn resolve_edge_channel_artifact(cfg: &Config, run_id: &str) -> Result<String, AppError> {
    let mut page = 1u32;
    let mut artifacts = Vec::new();

    loop {
        let url = format!(
            "https://api.github.com/repos/{}/{}/actions/runs/{}/artifacts?per_page={}&page={}",
            cfg.github_owner, cfg.github_repo, run_id, PER_PAGE, page,
        );
        let response = send_get(cfg, &url).await?;
        let status = response.status();

        if status == StatusCode::NOT_FOUND {
            return Err(AppError::not_found());
        }
        if !status.is_success() {
            return Err(AppError::bad_gateway(format!(
                "GitHub API request failed: {status}"
            )));
        }

        let body = read_body_to_vec(response, 10 * 1024 * 1024).await?;
        let payload: ArtifactListResponse = serde_json::from_slice(&body)
            .map_err(|err| AppError::bad_gateway(format!("invalid JSON: {err}")))?;
        let count = payload.artifacts.len();

        for artifact in payload.artifacts {
            if artifact.expired != Some(true) && artifact.name == GHA_EDGE_CHANNEL_ARTIFACT_NAME {
                artifacts.push(artifact);
            }
        }

        if count < PER_PAGE as usize {
            break;
        }
        page += 1;
    }

    if artifacts.is_empty() {
        return Err(AppError::new(
            StatusCode::NOT_FOUND,
            format!("no active {GHA_EDGE_CHANNEL_ARTIFACT_NAME} artifact found for run {run_id}"),
        ));
    }
    if artifacts.len() != 1 {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            format!(
                "run {run_id} has {} active {GHA_EDGE_CHANNEL_ARTIFACT_NAME} artifacts; expected exactly 1",
                artifacts.len(),
            ),
        ));
    }

    artifacts
        .remove(0)
        .archive_download_url
        .ok_or_else(|| AppError::bad_gateway("artifact is missing archive_download_url"))
}

async fn release_materialize(cfg: &Config) -> Result<(), AppError> {
    let release_dir = cfg.release_cache_dir();
    let target_dir = release_dir.join(&cfg.edge_artifact_id);
    let channel_path = target_dir.join(&cfg.asset_name);
    let sha256_path = target_dir.join(&cfg.sha256_asset_name);

    if channel_path.exists() && sha256_path.exists() {
        return Ok(());
    }

    let release_lock = RELEASE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = release_lock.lock().await;

    if channel_path.exists() && sha256_path.exists() {
        return Ok(());
    }

    fs::create_dir_all(&release_dir).map_err(io_500)?;
    fs::create_dir_all(&target_dir).map_err(io_500)?;

    let tmp_dir = create_temp_dir(&release_dir, &format!("release-{}", cfg.edge_artifact_id))?;
    let mut tmp_guard = TmpDirGuard::new(tmp_dir.clone());
    let zip_path = tmp_dir.join("artifact.zip");
    let channel_tmp = tmp_dir.join(&cfg.asset_name);
    let sha256_tmp = tmp_dir.join(&cfg.sha256_asset_name);
    let url = format!(
        "https://api.github.com/repos/{}/{}/actions/artifacts/{}/zip",
        cfg.github_owner, cfg.github_repo, cfg.edge_artifact_id,
    );

    download_archive(cfg, &url, &zip_path).await?;
    extract_named_file_from_zip(&zip_path, &cfg.asset_name, &channel_tmp)?;
    extract_named_file_from_zip(&zip_path, &cfg.sha256_asset_name, &sha256_tmp)?;

    fs::rename(&channel_tmp, &channel_path).map_err(io_500)?;
    fs::rename(&sha256_tmp, &sha256_path).map_err(io_500)?;

    tmp_guard.defuse();
    let _ = fs::remove_dir_all(&tmp_dir);

    Ok(())
}

async fn download_archive(cfg: &Config, url: &str, dest: &Path) -> Result<(), AppError> {
    let mut url = url.to_string();
    let mut redirects = 0usize;
    let mut temp_path = dest.as_os_str().to_owned();
    temp_path.push(".downloading");
    let temp_path = PathBuf::from(temp_path);

    loop {
        let response = send_get(cfg, &url).await?;
        let status = response.status();

        if is_redirect_status(status) {
            redirects += 1;
            if redirects > 5 {
                return Err(AppError::bad_gateway(
                    "too many redirects while downloading artifact",
                ));
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    AppError::bad_gateway("redirect response missing location header")
                })?;
            debug!(from = %url, to = %location, redirect_count = redirects, "followed artifact redirect");
            url = resolve_redirect_url(&url, location)?;
            continue;
        }

        if status == StatusCode::NOT_FOUND {
            return Err(AppError::not_found());
        }
        if !status.is_success() {
            return Err(AppError::bad_gateway(format!("download failed: {status}")));
        }

        if let Some(len) = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            && cfg.max_download_bytes > 0
            && len > cfg.max_download_bytes
        {
            return Err(AppError::bad_gateway(format!(
                "artifact exceeds max download size ({len} bytes)"
            )));
        }

        let mut file = fs::File::create(&temp_path).map_err(io_500)?;
        let mut total_bytes = 0u64;
        let mut body = response.into_body().into_boxed_body();

        loop {
            match BodyExt::frame(&mut body).await {
                Some(Ok(frame)) => {
                    if let Some(data) = frame.data_ref() {
                        total_bytes = total_bytes.saturating_add(data.len() as u64);
                        if cfg.max_download_bytes > 0 && total_bytes > cfg.max_download_bytes {
                            let _ = fs::remove_file(&temp_path);
                            return Err(AppError::bad_gateway(
                                "artifact exceeds max download size",
                            ));
                        }
                        if let Err(err) = file.write_all(data) {
                            let _ = fs::remove_file(&temp_path);
                            return Err(io_500(err));
                        }
                    }
                }
                Some(Err(err)) => {
                    let _ = fs::remove_file(&temp_path);
                    return Err(AppError::bad_gateway(format!(
                        "download stream failed: {err}"
                    )));
                }
                None => break,
            }
        }

        if let Err(err) = file.flush() {
            let _ = fs::remove_file(&temp_path);
            return Err(io_500(err));
        }
        if let Err(err) = fs::rename(&temp_path, dest) {
            let _ = fs::remove_file(&temp_path);
            return Err(io_500(err));
        }
        return Ok(());
    }
}

async fn send_get(cfg: &Config, url: &str) -> Result<Response<Body>, AppError> {
    let mut builder = Request::builder().method("GET").uri(url);

    if is_github_api_url(url) {
        builder = builder
            .header("user-agent", USER_AGENT)
            .header("x-github-api-version", GH_API_VERSION)
            .header("accept", "application/vnd.github+json");
        if !cfg.github_token.is_empty() {
            builder = builder.header("authorization", format!("Bearer {}", cfg.github_token));
        }
    }

    let request = builder
        .body(Body::empty())
        .map_err(|err| AppError::internal(format!("failed to build request: {err}")))?;

    Client::new()
        .send(request)
        .await
        .map_err(|err| AppError::bad_gateway(format!("request failed: {err}")))
}

async fn read_body_to_vec(response: Response<Body>, max_bytes: u64) -> Result<Vec<u8>, AppError> {
    let mut body = response.into_body().into_boxed_body();
    let mut bytes = Vec::new();
    let mut total_bytes = 0u64;

    loop {
        match BodyExt::frame(&mut body).await {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    total_bytes = total_bytes.saturating_add(data.len() as u64);
                    if max_bytes > 0 && total_bytes > max_bytes {
                        return Err(AppError::bad_gateway("response body exceeds max size"));
                    }
                    bytes.extend_from_slice(data);
                }
            }
            Some(Err(err)) => {
                return Err(AppError::bad_gateway(format!(
                    "response body read failed: {err}"
                )));
            }
            None => return Ok(bytes),
        }
    }
}

fn extract_single_file_from_zip(
    zip_path: &Path,
    blob_path: &Path,
) -> Result<(String, String), AppError> {
    let file = fs::File::open(zip_path).map_err(io_500)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| AppError::bad_gateway("artifact archive is not a valid zip file"))?;
    let mut file_indices = Vec::new();
    let mut all_names = Vec::new();

    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|_| AppError::bad_gateway("artifact archive is not a valid zip file"))?;
        let name = entry.name().to_string();
        if !entry.is_dir() && !name.ends_with(".sha256") && !name.ends_with(".sha256sum") {
            file_indices.push(i);
        }
        all_names.push(name);
    }

    if file_indices.len() != 1 {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            format!(
                "artifact archive contains {} non-checksum files; expected exactly 1 (files: {})",
                file_indices.len(),
                all_names.join(", "),
            ),
        ));
    }

    let mut entry = archive
        .by_index(file_indices[0])
        .map_err(|_| AppError::bad_gateway("artifact archive is not a valid zip file"))?;
    let filename = entry.name().to_string();
    let mut out = fs::File::create(blob_path).map_err(io_500)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];

    loop {
        let n = entry
            .read(&mut buf)
            .map_err(|err| AppError::bad_gateway(format!("read zip: {err}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(io_500)?;
    }

    out.flush().map_err(io_500)?;
    let etag = format!("\"sha256-{}\"", hex_encode(&hasher.finalize()));
    let content_type = content_type_for_filename(&filename).to_string();
    Ok((content_type, etag))
}

fn extract_named_file_from_zip(
    zip_path: &Path,
    name: &str,
    dest: &Path,
) -> Result<(), AppError> {
    let file = fs::File::open(zip_path).map_err(io_500)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| AppError::bad_gateway("artifact archive is not a valid zip file"))?;
    let mut found = None;

    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|_| AppError::bad_gateway("artifact archive is not a valid zip file"))?;
        if !entry.is_dir() {
            let entry_name = Path::new(entry.name())
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if entry_name == name {
                found = Some(i);
                break;
            }
        }
    }

    let idx = found
        .ok_or_else(|| AppError::bad_gateway(format!("artifact archive is missing {name}")))?;
    let mut entry = archive
        .by_index(idx)
        .map_err(|_| AppError::bad_gateway("artifact archive is not a valid zip file"))?;
    let mut out = fs::File::create(dest).map_err(io_500)?;
    let mut buf = vec![0u8; CHUNK_SIZE];

    loop {
        let n = entry
            .read(&mut buf)
            .map_err(|err| AppError::bad_gateway(format!("read zip: {err}")))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(io_500)?;
    }

    out.flush().map_err(io_500)?;
    Ok(())
}

enum CorsPolicy {
    GhaWildcard,
    Release(String),
}

struct FileResponse<'a> {
    path: &'a Path,
    size: u64,
    content_type: &'a str,
    cache_control: &'a str,
    etag: Option<&'a str>,
    cors: CorsPolicy,
    head: bool,
    range_header: Option<&'a str>,
}

fn serve_file(file: FileResponse<'_>) -> Response<Body> {
    let (status, start, end, length) = if let Some(range) = file.range_header {
        match parse_single_byte_range(range, file.size) {
            Ok((start, end)) => (StatusCode::PARTIAL_CONTENT, start, end, end - start + 1),
            Err(_) => {
                let mut headers = base_file_headers(file.cache_control, "0");
                headers.push(("accept-ranges", "bytes".to_string()));
                headers.push(("content-range", format!("bytes */{}", file.size)));
                append_cors_headers(&mut headers, file.cors);
                return response_with_headers(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    headers,
                    Body::empty(),
                );
            }
        }
    } else if file.size == 0 {
        (StatusCode::OK, 0, 0, 0)
    } else {
        (StatusCode::OK, 0, file.size - 1, file.size)
    };

    let mut headers = base_file_headers(file.cache_control, &length.to_string());
    headers.push(("accept-ranges", "bytes".to_string()));
    headers.push(("content-type", file.content_type.to_string()));
    if let Some(etag) = file.etag {
        headers.push(("etag", etag.to_string()));
    }
    if status == StatusCode::PARTIAL_CONTENT {
        headers.push((
            "content-range",
            format!("bytes {start}-{end}/{}", file.size),
        ));
    }
    append_cors_headers(&mut headers, file.cors);

    if file.head || length == 0 {
        return response_with_headers(status, headers, Body::empty());
    }

    match file_range_body(file.path, start, length) {
        Ok(body) => response_with_headers(status, headers, body),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            text_response(StatusCode::NOT_FOUND, "not found\n")
        }
        Err(err) => {
            error!(path = ?file.path, error = %err, "failed to read cached file");
            text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to read cached file\n",
            )
        }
    }
}

fn base_file_headers(cache_control: &str, content_length: &str) -> Vec<(&'static str, String)> {
    vec![
        ("cache-control", cache_control.to_string()),
        ("content-length", content_length.to_string()),
    ]
}

fn append_cors_headers(headers: &mut Vec<(&'static str, String)>, cors: CorsPolicy) {
    match cors {
        CorsPolicy::GhaWildcard => {
            headers.push(("access-control-allow-origin", "*".to_string()));
            headers.push((
                "access-control-allow-methods",
                "GET, HEAD, OPTIONS".to_string(),
            ));
            headers.push((
                "access-control-allow-headers",
                "Content-Type, Range".to_string(),
            ));
            headers.push((
                "access-control-expose-headers",
                "Content-Length, Content-Range, ETag, Accept-Ranges".to_string(),
            ));
        }
        CorsPolicy::Release(origin) => {
            if origin.is_empty() || !is_allowed_release_origin(&origin) {
                return;
            }
            headers.push(("access-control-allow-origin", origin));
            headers.push(("vary", "Origin".to_string()));
            headers.push((
                "access-control-allow-methods",
                "GET, HEAD, OPTIONS".to_string(),
            ));
            headers.push((
                "access-control-allow-headers",
                "Content-Type, Range".to_string(),
            ));
            headers.push((
                "access-control-expose-headers",
                "Content-Length, Content-Range, ETag, Accept-Ranges".to_string(),
            ));
        }
    }
}

fn file_range_body(path: &Path, start: u64, length: u64) -> io::Result<Body> {
    let mut file = fs::File::open(path)?;
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    Ok(Body::from_try_stream(FileRangeStream {
        file,
        remaining: length,
    }))
}

struct FileRangeStream {
    file: fs::File,
    remaining: u64,
}

impl futures_lite::Stream for FileRangeStream {
    type Item = io::Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.remaining == 0 {
            return Poll::Ready(None);
        }

        let this = self.as_mut().get_mut();
        let len = this.remaining.min(CHUNK_SIZE as u64) as usize;
        let mut buf = vec![0u8; len];
        match this.file.read(&mut buf) {
            Ok(0) => {
                this.remaining = 0;
                Poll::Ready(None)
            }
            Ok(n) => {
                this.remaining = this.remaining.saturating_sub(n as u64);
                buf.truncate(n);
                Poll::Ready(Some(Ok(buf)))
            }
            Err(err) => {
                this.remaining = 0;
                Poll::Ready(Some(Err(err)))
            }
        }
    }
}

fn gha_options_response() -> Response<Body> {
    response_with_headers(
        StatusCode::NO_CONTENT,
        vec![
            ("access-control-allow-origin", "*".to_string()),
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
            ("access-control-max-age", "86400".to_string()),
            ("content-length", "0".to_string()),
        ],
        Body::empty(),
    )
}

fn release_options_response(origin: &str) -> Response<Body> {
    let mut headers = vec![
        ("access-control-max-age", "86400".to_string()),
        ("content-length", "0".to_string()),
    ];
    append_cors_headers(&mut headers, CorsPolicy::Release(origin.to_string()));
    response_with_headers(StatusCode::NO_CONTENT, headers, Body::empty())
}

fn text_response(status: StatusCode, text: &str) -> Response<Body> {
    response_with_headers(
        status,
        vec![
            ("content-type", "text/plain; charset=utf-8".to_string()),
            ("cache-control", "no-store".to_string()),
            ("content-length", text.len().to_string()),
        ],
        Body::from(text.to_string()),
    )
}

fn error_response(err: AppError) -> Response<Body> {
    text_response(err.status, &format!("{}\n", err.message))
}

fn method_not_allowed_response() -> Response<Body> {
    text_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n")
}

fn response_with_headers(
    status: StatusCode,
    headers: Vec<(&'static str, String)>,
    body: Body,
) -> Response<Body> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder.body(body).expect("response should build")
}

fn content_type_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    content_type_for_ext(ext)
}

fn content_type_for_filename(filename: &str) -> &'static str {
    content_type_for_path(Path::new(filename))
}

fn is_redirect_status(status: StatusCode) -> bool {
    status == StatusCode::MOVED_PERMANENTLY
        || status == StatusCode::FOUND
        || status == StatusCode::SEE_OTHER
        || status == StatusCode::TEMPORARY_REDIRECT
        || status == StatusCode::PERMANENT_REDIRECT
}

fn is_github_api_url(url: &str) -> bool {
    url.starts_with("https://api.github.com/")
}

fn resolve_redirect_url(current_url: &str, location: &str) -> Result<String, AppError> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Ok(location.to_string());
    }

    if location.starts_with('/') {
        let scheme_end = current_url
            .find("://")
            .ok_or_else(|| AppError::bad_gateway("redirect base URL is invalid"))?;
        let authority_start = scheme_end + 3;
        let authority_end = current_url[authority_start..]
            .find('/')
            .map(|idx| authority_start + idx)
            .unwrap_or(current_url.len());
        return Ok(format!("{}{}", &current_url[..authority_end], location));
    }

    Err(AppError::bad_gateway("redirect location is not absolute"))
}

fn create_temp_dir(parent: &Path, prefix: &str) -> Result<PathBuf, AppError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| AppError::internal(format!("failed to read system time: {err}")))?
        .as_nanos();

    for attempt in 0..100u32 {
        let candidate = parent.join(format!(".tmp-{prefix}-{nanos}-{attempt}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(io_500(err)),
        }
    }

    Err(AppError::internal("failed to allocate unique temp dir"))
}

fn io_500(err: io::Error) -> AppError {
    AppError::internal(format!("filesystem operation failed: {err}"))
}
