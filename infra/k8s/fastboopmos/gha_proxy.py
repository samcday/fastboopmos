#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
import mimetypes
import os
import re
import shutil
import tempfile
import threading
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


CHUNK_SIZE = 1024 * 1024
PER_PAGE = 100
RUN_ID_RE = re.compile(r"^[0-9]+$")
PATH_RE = re.compile(r"^/gha/([0-9]+)/?$")
USER_AGENT = "fastboopmos-gha-proxy/0.1"


def env_int(name: str, default: int) -> int:
    raw = os.environ.get(name, "").strip()
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError as exc:
        raise RuntimeError(f"{name} must be an integer") from exc


PORT = env_int("PORT", 8080)
CACHE_DIR = Path(os.environ.get("CACHE_DIR", "/cache")).resolve()
CACHE_MAX_BYTES = env_int("CACHE_MAX_BYTES", 0)
REQUEST_TIMEOUT_SECONDS = env_int("REQUEST_TIMEOUT_SECONDS", 300)
GITHUB_OWNER = os.environ.get("GITHUB_OWNER", "samcday").strip()
GITHUB_REPO = os.environ.get("GITHUB_REPO", "fastboopmos").strip()
GITHUB_TOKEN = os.environ.get("GITHUB_TOKEN", "").strip()


class ProxyError(Exception):
    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status
        self.message = message


@dataclass(frozen=True)
class CacheEntry:
    blob_path: Path
    meta_path: Path
    size: int
    content_type: str
    etag: str


_CACHE_LOCKS: dict[str, threading.Lock] = {}
_CACHE_LOCKS_GUARD = threading.Lock()


def github_headers() -> dict[str, str]:
    headers = {
        "accept": "application/vnd.github+json",
        "user-agent": USER_AGENT,
        "x-github-api-version": "2022-11-28",
    }
    if GITHUB_TOKEN:
        headers["authorization"] = f"Bearer {GITHUB_TOKEN}"
    return headers


def github_json(url: str) -> object:
    req = urllib.request.Request(url, method="GET", headers=github_headers())
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT_SECONDS) as response:
            return json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as err:
        status = 502
        if err.code == 404:
            status = 404
        raise ProxyError(status, f"GitHub API request failed: {err.code}") from err
    except urllib.error.URLError as err:
        raise ProxyError(502, f"GitHub API request failed: {err}") from err
    except ValueError as err:
        raise ProxyError(502, "GitHub API returned invalid JSON") from err


def cache_key(run_id: str) -> str:
    key = f"{GITHUB_OWNER}/{GITHUB_REPO}:{run_id}".encode("utf-8")
    return hashlib.sha256(key).hexdigest()


def cache_paths(key: str) -> tuple[Path, Path]:
    return CACHE_DIR / f"{key}.blob", CACHE_DIR / f"{key}.json"


def cache_lock_for(key: str) -> threading.Lock:
    with _CACHE_LOCKS_GUARD:
        lock = _CACHE_LOCKS.get(key)
        if lock is None:
            lock = threading.Lock()
            _CACHE_LOCKS[key] = lock
    return lock


def resolve_single_artifact(run_id: str) -> dict[str, str]:
    owner = urllib.parse.quote(GITHUB_OWNER, safe="")
    repo = urllib.parse.quote(GITHUB_REPO, safe="")
    page = 1
    artifacts: list[dict[str, object]] = []
    while True:
        url = (
            f"https://api.github.com/repos/{owner}/{repo}"
            f"/actions/runs/{run_id}/artifacts?per_page={PER_PAGE}&page={page}"
        )
        payload = github_json(url)
        if not isinstance(payload, dict):
            raise ProxyError(502, "unexpected artifacts payload")

        page_items = payload.get("artifacts")
        if not isinstance(page_items, list):
            raise ProxyError(502, "GitHub API response missing artifacts")

        for artifact in page_items:
            if isinstance(artifact, dict) and not artifact.get("expired"):
                artifacts.append(artifact)

        if len(page_items) < PER_PAGE:
            break
        page += 1

    if not artifacts:
        raise ProxyError(404, f"no active artifacts found for run {run_id}")
    if len(artifacts) != 1:
        raise ProxyError(
            409,
            f"run {run_id} has {len(artifacts)} active artifacts; expected exactly 1",
        )

    artifact = artifacts[0]
    archive_url = artifact.get("archive_download_url")
    if not isinstance(archive_url, str) or not archive_url:
        raise ProxyError(502, "artifact is missing archive_download_url")
    return {"archive_download_url": archive_url}


def load_cache_entry(blob_path: Path, meta_path: Path) -> CacheEntry | None:
    if not blob_path.exists() or not meta_path.exists():
        return None
    try:
        metadata = json.loads(meta_path.read_text(encoding="utf-8"))
        return CacheEntry(
            blob_path=blob_path,
            meta_path=meta_path,
            size=blob_path.stat().st_size,
            content_type=str(metadata["content_type"]),
            etag=str(metadata["etag"]),
        )
    except Exception:
        return None


def download_archive(url: str, destination: Path) -> None:
    req = urllib.request.Request(url, method="GET", headers=github_headers())
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT_SECONDS) as response:
            with destination.open("wb") as output:
                shutil.copyfileobj(response, output, CHUNK_SIZE)
    except urllib.error.HTTPError as err:
        status = 502
        if err.code == 404:
            status = 404
        raise ProxyError(status, f"failed to download artifact archive: {err.code}") from err
    except urllib.error.URLError as err:
        raise ProxyError(502, f"failed to download artifact archive: {err}") from err


def extract_single_file(zip_path: Path, blob_tmp_path: Path) -> tuple[int, str, str]:
    try:
        with zipfile.ZipFile(zip_path) as archive:
            entries = [entry for entry in archive.infolist() if not entry.is_dir()]
            if len(entries) != 1:
                raise ProxyError(
                    409,
                    f"artifact archive contains {len(entries)} files; expected exactly 1",
                )
            entry = entries[0]
            digest = hashlib.sha256()
            with archive.open(entry, "r") as source, blob_tmp_path.open("wb") as output:
                while True:
                    chunk = source.read(CHUNK_SIZE)
                    if not chunk:
                        break
                    digest.update(chunk)
                    output.write(chunk)
            size = blob_tmp_path.stat().st_size
            content_type, _ = mimetypes.guess_type(entry.filename)
            if not content_type:
                content_type = "application/octet-stream"
            etag = f'"sha256-{digest.hexdigest()}"'
            return size, content_type, etag
    except zipfile.BadZipFile as err:
        raise ProxyError(502, "artifact archive is not a valid zip file") from err


def enforce_cache_limit() -> None:
    if CACHE_MAX_BYTES <= 0:
        return

    entries: list[tuple[float, int, Path, Path]] = []
    total_bytes = 0
    for blob_path in CACHE_DIR.glob("*.blob"):
        meta_path = blob_path.with_suffix(".json")
        if not meta_path.exists():
            continue
        try:
            stat = blob_path.stat()
        except OSError:
            continue
        total_bytes += stat.st_size
        entries.append((stat.st_mtime, stat.st_size, blob_path, meta_path))

    entries.sort(key=lambda item: item[0])
    while total_bytes > CACHE_MAX_BYTES and len(entries) > 1:
        _, size, blob_path, meta_path = entries.pop(0)
        blob_path.unlink(missing_ok=True)
        meta_path.unlink(missing_ok=True)
        total_bytes -= size


def materialize_cache_entry(run_id: str) -> CacheEntry:
    key = cache_key(run_id)
    blob_path, meta_path = cache_paths(key)
    existing = load_cache_entry(blob_path, meta_path)
    if existing is not None:
        return existing

    lock = cache_lock_for(key)
    with lock:
        existing = load_cache_entry(blob_path, meta_path)
        if existing is not None:
            return existing

        artifact = resolve_single_artifact(run_id)
        archive_url = artifact["archive_download_url"]

        with tempfile.TemporaryDirectory(prefix=f".gha-{key}-", dir=CACHE_DIR) as tmpdir:
            tmpdir_path = Path(tmpdir)
            zip_path = tmpdir_path / "artifact.zip"
            blob_tmp_path = tmpdir_path / "blob.tmp"
            meta_tmp_path = tmpdir_path / "meta.tmp"

            download_archive(archive_url, zip_path)
            size, content_type, etag = extract_single_file(zip_path, blob_tmp_path)
            meta_tmp_path.write_text(
                json.dumps(
                    {
                        "size": size,
                        "content_type": content_type,
                        "etag": etag,
                    },
                    separators=(",", ":"),
                ),
                encoding="utf-8",
            )

            blob_tmp_path.replace(blob_path)
            meta_tmp_path.replace(meta_path)

        enforce_cache_limit()

    created = load_cache_entry(blob_path, meta_path)
    if created is None:
        raise ProxyError(500, "failed to load cached artifact after extraction")
    return created


class GhaProxyHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "fastboopmos-gha-proxy/0.1"

    def do_OPTIONS(self) -> None:
        path = urllib.parse.urlsplit(self.path).path
        if path == "/gha" or path.startswith("/gha/"):
            self.send_response(204)
            self.write_cors_headers()
            self.send_header("access-control-max-age", "86400")
            self.send_header("content-length", "0")
            self.end_headers()
            return
        self.send_error_response(404, "not found", head_only=True)

    def do_HEAD(self) -> None:
        self.handle_request(head_only=True)

    def do_GET(self) -> None:
        self.handle_request(head_only=False)

    def handle_request(self, head_only: bool) -> None:
        path = urllib.parse.urlsplit(self.path).path
        if path == "/healthz":
            self.send_response(200)
            self.send_header("cache-control", "no-store")
            self.send_header("content-type", "text/plain; charset=utf-8")
            self.send_header("content-length", "2")
            self.end_headers()
            if not head_only:
                self.wfile.write(b"ok")
            return

        match = PATH_RE.fullmatch(path)
        if match is None:
            self.send_error_response(404, "not found", head_only=head_only)
            return
        run_id = match.group(1)
        if RUN_ID_RE.fullmatch(run_id) is None:
            self.send_error_response(404, "not found", head_only=head_only)
            return

        try:
            entry = materialize_cache_entry(run_id)
        except ProxyError as err:
            self.send_error_response(err.status, err.message, head_only=head_only)
            return

        self.send_response(200)
        self.write_cors_headers()
        self.send_header("cache-control", "public, max-age=31536000, immutable")
        self.send_header("content-length", str(entry.size))
        self.send_header("content-type", entry.content_type)
        self.send_header("etag", entry.etag)
        self.end_headers()

        if head_only:
            return

        try:
            with entry.blob_path.open("rb") as source:
                shutil.copyfileobj(source, self.wfile, CHUNK_SIZE)
        except BrokenPipeError:
            return

    def write_cors_headers(self) -> None:
        self.send_header("access-control-allow-origin", "*")
        self.send_header("access-control-allow-methods", "GET, HEAD, OPTIONS")
        self.send_header("access-control-allow-headers", "Content-Type")
        self.send_header("access-control-expose-headers", "Content-Length, ETag")

    def send_error_response(self, status: int, message: str, *, head_only: bool) -> None:
        body = f"{message}\n".encode("utf-8")
        self.send_response(status)
        self.write_cors_headers()
        self.send_header("cache-control", "no-store")
        self.send_header("content-type", "text/plain; charset=utf-8")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        if not head_only:
            self.wfile.write(body)


def main() -> None:
    if not GITHUB_OWNER or not GITHUB_REPO:
        raise RuntimeError("GITHUB_OWNER and GITHUB_REPO must be configured")
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(("0.0.0.0", PORT), GhaProxyHandler)
    server.serve_forever()


if __name__ == "__main__":
    main()
