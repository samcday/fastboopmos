#!/usr/bin/env python3

from __future__ import annotations

import mimetypes
import os
import shutil
import tempfile
import threading
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


CHUNK_SIZE = 1024 * 1024
USER_AGENT = "fastboopmos-release-app/0.2"

ALLOWED_ORIGIN_EXACT = {
    "https://www.fastboop.win",
    "https://bleeding.fastboop.win",
}
ALLOWED_LOCALHOST_HOSTS = {"localhost", "127.0.0.1"}


class RangeNotSatisfiableError(Exception):
    pass


class ReleaseError(Exception):
    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status
        self.message = message


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
REQUEST_TIMEOUT_SECONDS = env_int("REQUEST_TIMEOUT_SECONDS", 300)
GITHUB_OWNER = os.environ.get("GITHUB_OWNER", "samcday").strip()
GITHUB_REPO = os.environ.get("GITHUB_REPO", "fastboopmos").strip()
ASSET_NAME = os.environ.get("ASSET_NAME", "edge.channel").strip()
SHA256_ASSET_NAME = os.environ.get("SHA256_ASSET_NAME", "edge.channel.sha256").strip()
EDGE_CHANNEL_ARTIFACT_ID = os.environ.get("EDGE_CHANNEL_ARTIFACT_ID", "").strip()
GITHUB_TOKEN = os.environ.get("GITHUB_TOKEN", "").strip()

_ARTIFACT_LOCKS: dict[str, threading.Lock] = {}
_ARTIFACT_LOCKS_GUARD = threading.Lock()


def github_headers() -> dict[str, str]:
    headers = {
        "accept": "application/vnd.github+json",
        "user-agent": USER_AGENT,
        "x-github-api-version": "2022-11-28",
    }
    if GITHUB_TOKEN:
        headers["authorization"] = f"Bearer {GITHUB_TOKEN}"
    return headers


def artifact_archive_url(artifact_id: str) -> str:
    owner = urllib.parse.quote(GITHUB_OWNER, safe="")
    repo = urllib.parse.quote(GITHUB_REPO, safe="")
    return f"https://api.github.com/repos/{owner}/{repo}/actions/artifacts/{artifact_id}/zip"


def artifact_lock(artifact_id: str) -> threading.Lock:
    with _ARTIFACT_LOCKS_GUARD:
        lock = _ARTIFACT_LOCKS.get(artifact_id)
        if lock is None:
            lock = threading.Lock()
            _ARTIFACT_LOCKS[artifact_id] = lock
    return lock


def asset_path(artifact_id: str, asset_name: str) -> Path:
    return CACHE_DIR / artifact_id / asset_name


def download_archive(artifact_id: str, destination: Path) -> None:
    req = urllib.request.Request(
        artifact_archive_url(artifact_id),
        method="GET",
        headers=github_headers(),
    )
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT_SECONDS) as response:
            with destination.open("wb") as output:
                shutil.copyfileobj(response, output, CHUNK_SIZE)
    except urllib.error.HTTPError as err:
        status = 502
        if err.code == 404:
            status = 404
        raise ReleaseError(status, f"failed to download artifact archive: {err.code}") from err
    except urllib.error.URLError as err:
        raise ReleaseError(502, f"failed to download artifact archive: {err}") from err


def extract_asset_from_zip(archive: zipfile.ZipFile, asset_name: str, destination: Path) -> None:
    members = [member for member in archive.infolist() if not member.is_dir()]
    selected = next((m for m in members if Path(m.filename).name == asset_name), None)
    if selected is None:
        raise ReleaseError(502, f"artifact archive is missing {asset_name}")

    with archive.open(selected, "r") as source, destination.open("wb") as output:
        shutil.copyfileobj(source, output, CHUNK_SIZE)


def materialize_assets(artifact_id: str) -> None:
    target_dir = CACHE_DIR / artifact_id
    target_channel = target_dir / ASSET_NAME
    target_sha256 = target_dir / SHA256_ASSET_NAME
    if target_channel.is_file() and target_sha256.is_file():
        return

    lock = artifact_lock(artifact_id)
    with lock:
        if target_channel.is_file() and target_sha256.is_file():
            return

        target_dir.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=f".edge-{artifact_id}-", dir=CACHE_DIR) as tmpdir:
            tmpdir_path = Path(tmpdir)
            zip_path = tmpdir_path / "artifact.zip"
            channel_tmp = tmpdir_path / ASSET_NAME
            sha256_tmp = tmpdir_path / SHA256_ASSET_NAME

            download_archive(artifact_id, zip_path)
            try:
                with zipfile.ZipFile(zip_path) as archive:
                    extract_asset_from_zip(archive, ASSET_NAME, channel_tmp)
                    extract_asset_from_zip(archive, SHA256_ASSET_NAME, sha256_tmp)
            except zipfile.BadZipFile as err:
                raise ReleaseError(502, "artifact archive is not a valid zip file") from err

            channel_tmp.replace(target_channel)
            sha256_tmp.replace(target_sha256)


def materialize_asset(asset_name: str) -> Path:
    materialize_assets(EDGE_CHANNEL_ARTIFACT_ID)
    return asset_path(EDGE_CHANNEL_ARTIFACT_ID, asset_name)


def content_type_for(path: Path) -> str:
    guessed, _ = mimetypes.guess_type(path.name)
    if guessed:
        return guessed
    return "application/octet-stream"


def is_allowed_origin(origin: str) -> bool:
    if origin in ALLOWED_ORIGIN_EXACT:
        return True

    parsed = urllib.parse.urlsplit(origin)
    if parsed.scheme not in {"http", "https"}:
        return False
    if parsed.hostname not in ALLOWED_LOCALHOST_HOSTS:
        return False
    return True


def parse_single_byte_range(range_header: str, size: int) -> tuple[int, int]:
    if not range_header.startswith("bytes="):
        raise RangeNotSatisfiableError("unsupported range unit")
    spec = range_header[len("bytes=") :].strip()
    if not spec or "," in spec:
        raise RangeNotSatisfiableError("multiple ranges are not supported")
    if "-" not in spec:
        raise RangeNotSatisfiableError("invalid range syntax")

    start_raw, end_raw = spec.split("-", 1)
    start_raw = start_raw.strip()
    end_raw = end_raw.strip()

    if not start_raw:
        if not end_raw:
            raise RangeNotSatisfiableError("invalid suffix range")
        try:
            suffix_len = int(end_raw)
        except ValueError as exc:
            raise RangeNotSatisfiableError("invalid suffix range") from exc
        if suffix_len <= 0:
            raise RangeNotSatisfiableError("invalid suffix length")
        if suffix_len >= size:
            return 0, size - 1
        return size - suffix_len, size - 1

    try:
        start = int(start_raw)
    except ValueError as exc:
        raise RangeNotSatisfiableError("invalid range start") from exc
    if start < 0 or start >= size:
        raise RangeNotSatisfiableError("range start out of bounds")

    if not end_raw:
        return start, size - 1

    try:
        end = int(end_raw)
    except ValueError as exc:
        raise RangeNotSatisfiableError("invalid range end") from exc
    if end < start:
        raise RangeNotSatisfiableError("range end before start")
    if end >= size:
        end = size - 1
    return start, end


class ReleaseHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "fastboopmos-release-app/0.2"

    def do_HEAD(self) -> None:
        self.handle_request(head_only=True)

    def do_OPTIONS(self) -> None:
        path = urllib.parse.urlsplit(self.path).path
        if path in {"/edge.channel", "/edge.channel.sha256", "/__fastboopmos/live"}:
            self.send_response(204)
            self.write_cors_headers()
            self.send_header("access-control-max-age", "86400")
            self.send_header("content-length", "0")
            self.end_headers()
            return
        self.send_text(404, "not found\n", head_only=True)

    def do_GET(self) -> None:
        self.handle_request(head_only=False)

    def handle_request(self, head_only: bool) -> None:
        path = urllib.parse.urlsplit(self.path).path
        if path == "/healthz":
            self.send_text(200, "ok\n", head_only=head_only)
            return

        if path == "/__fastboopmos/live":
            self.send_text(200, f"artifact_id={EDGE_CHANNEL_ARTIFACT_ID}\n", head_only=head_only)
            return

        if path == "/edge.channel":
            asset_name = ASSET_NAME
        elif path == "/edge.channel.sha256":
            asset_name = SHA256_ASSET_NAME
        else:
            self.send_text(404, "not found\n", head_only=head_only)
            return

        try:
            asset = materialize_asset(asset_name)
            size = asset.stat().st_size
        except ReleaseError as err:
            self.send_text(err.status, f"{err.message}\n", head_only=head_only)
            return
        except OSError:
            self.send_text(404, "not found\n", head_only=head_only)
            return

        range_header = self.headers.get("range")
        start = 0
        end = size - 1
        status = 200
        if range_header:
            try:
                start, end = parse_single_byte_range(range_header, size)
                status = 206
            except RangeNotSatisfiableError:
                self.send_response(416)
                self.write_cors_headers()
                self.send_header("cache-control", "no-store")
                self.send_header("accept-ranges", "bytes")
                self.send_header("content-range", f"bytes */{size}")
                self.send_header("content-length", "0")
                self.end_headers()
                return

        length = end - start + 1

        self.send_response(status)
        self.write_cors_headers()
        self.send_header("cache-control", "no-store")
        self.send_header("accept-ranges", "bytes")
        self.send_header("content-type", content_type_for(asset))
        self.send_header("content-length", str(length))
        if status == 206:
            self.send_header("content-range", f"bytes {start}-{end}/{size}")
        self.end_headers()

        if head_only:
            return

        try:
            with asset.open("rb") as source:
                if start:
                    source.seek(start)
                remaining = length
                while remaining > 0:
                    chunk = source.read(min(CHUNK_SIZE, remaining))
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    remaining -= len(chunk)
        except BrokenPipeError:
            return

    def send_text(self, status: int, body: str, *, head_only: bool) -> None:
        encoded = body.encode("utf-8")
        self.send_response(status)
        self.write_cors_headers()
        self.send_header("cache-control", "no-store")
        self.send_header("content-type", "text/plain; charset=utf-8")
        self.send_header("content-length", str(len(encoded)))
        self.end_headers()
        if not head_only:
            self.wfile.write(encoded)

    def write_cors_headers(self) -> None:
        origin = self.headers.get("origin", "").strip()
        if not origin or not is_allowed_origin(origin):
            return

        self.send_header("access-control-allow-origin", origin)
        self.send_header("vary", "Origin")
        self.send_header("access-control-allow-methods", "GET, HEAD, OPTIONS")
        self.send_header("access-control-allow-headers", "Content-Type, Range")
        self.send_header(
            "access-control-expose-headers",
            "Content-Length, Content-Range, ETag, Accept-Ranges",
        )


def main() -> None:
    if not GITHUB_OWNER or not GITHUB_REPO:
        raise RuntimeError("GITHUB_OWNER and GITHUB_REPO must be configured")
    if not EDGE_CHANNEL_ARTIFACT_ID or not EDGE_CHANNEL_ARTIFACT_ID.isdigit():
        raise RuntimeError("EDGE_CHANNEL_ARTIFACT_ID must be a numeric GitHub artifact id")
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(("0.0.0.0", PORT), ReleaseHandler)
    server.serve_forever()


if __name__ == "__main__":
    main()
