#!/usr/bin/env python3

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlparse

from jinja2 import Environment, StrictUndefined


@dataclass(frozen=True)
class RootfsSelection:
    pmos_device: str
    ui_name: str
    variant: str | None
    image_name: str
    image_url: str
    image_sha512: str
    image_size: int
    timestamp: str

    @property
    def target_name(self) -> str:
        if self.variant is None:
            return self.pmos_device
        return f"{self.pmos_device}-{self.variant}"


@dataclass(frozen=True)
class CacheConfig:
    bucket: str
    endpoint_url: str | None
    prefix: str


def fastboop_version(fastboop: str) -> str:
    completed = subprocess.run(
        [fastboop, "--version"],
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise RuntimeError(f"failed to determine fastboop version from {fastboop}")
    lines = completed.stdout.strip().splitlines()
    if not lines:
        raise RuntimeError("fastboop --version returned empty output")
    return lines[0].strip()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build edge.channel from pmOS index + device templates + cached bootpros."
    )
    parser.add_argument(
        "--templates-dir",
        default=".",
        help="Directory containing per-device template YAML files",
    )
    parser.add_argument(
        "--only-device",
        help="Optional postmarketOS device id to process (e.g. oneplus-fajita)",
    )
    parser.add_argument(
        "--index-url",
        default="https://images.postmarketos.org/bpo/index.json",
        help="postmarketOS index.json URL",
    )
    parser.add_argument(
        "--release",
        default="edge",
        help="postmarketOS release name",
    )
    parser.add_argument(
        "--fastboop",
        default="fastboop",
        help="Path to fastboop CLI binary",
    )
    parser.add_argument(
        "--cache-bucket",
        required=True,
        help="S3-compatible bucket for cached compiled .bootpro artifacts",
    )
    parser.add_argument(
        "--cache-endpoint-url",
        default="",
        help="Optional S3 endpoint URL",
    )
    parser.add_argument(
        "--cache-prefix",
        default="fastboopmos",
        help="Object key prefix for cached bootpros",
    )
    parser.add_argument(
        "--artifact-cache-dir",
        default="build/pmos-artifacts",
        help="Local cache directory for source .img.xz artifacts",
    )
    parser.add_argument(
        "--bootpro-cache-dir",
        default="build/pmos-bootpros",
        help="Local cache directory for compiled .bootpro artifacts",
    )
    parser.add_argument(
        "--output",
        default="dist/edge.channel",
        help="Output channel path",
    )
    return parser.parse_args()


def normalize_prefix(prefix: str) -> str:
    stripped = prefix.strip("/")
    if not stripped:
        raise ValueError("cache prefix must not be empty")
    return stripped


def aws_base_args(cache: CacheConfig) -> list[str]:
    args = ["aws", "s3api"]
    if cache.endpoint_url:
        args.extend(["--endpoint-url", cache.endpoint_url])
    return args


def s3_head_object(cache: CacheConfig, key: str) -> bool:
    completed = subprocess.run(
        aws_base_args(cache)
        + ["head-object", "--bucket", cache.bucket, "--key", key],
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode == 0:
        return True

    stderr_lower = completed.stderr.lower()
    if "not found" in stderr_lower or "404" in stderr_lower:
        return False
    raise RuntimeError(
        f"aws s3api head-object --bucket {cache.bucket} --key {key} failed: {completed.stderr.strip()}"
    )


def s3_get_object(cache: CacheConfig, key: str, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    completed = subprocess.run(
        aws_base_args(cache)
        + [
            "get-object",
            "--bucket",
            cache.bucket,
            "--key",
            key,
            str(destination),
        ],
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"aws s3api get-object --bucket {cache.bucket} --key {key} failed: {completed.stderr.strip()}"
        )


def s3_put_object(cache: CacheConfig, key: str, source: Path) -> None:
    completed = subprocess.run(
        aws_base_args(cache)
        + [
            "put-object",
            "--bucket",
            cache.bucket,
            "--key",
            key,
            "--body",
            str(source),
        ],
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"aws s3api put-object --bucket {cache.bucket} --key {key} failed: {completed.stderr.strip()}"
        )


def bootpro_scope_hash(manifest_content: str, fastboop_ver: str) -> str:
    payload = f"{fastboop_ver}\n{manifest_content}".encode("utf-8")
    return hashlib.sha256(payload).hexdigest()[:24]


def cache_key(prefix: str, release: str, image_sha512: str, scope_hash: str) -> str:
    return f"{prefix}/{release}/bootpro/{image_sha512}-{scope_hash}.bootpro"


def rootfs_variant(image_name: str, pmos_device: str) -> str | None:
    if not image_name.endswith(".img.xz"):
        return None
    if image_name.endswith("-boot.img.xz") or image_name.endswith("-bootpart.img.xz"):
        return None

    bare_suffix = f"-{pmos_device}.img.xz"
    if image_name.endswith(bare_suffix):
        return ""

    marker = f"-{pmos_device}-"
    index = image_name.rfind(marker)
    if index == -1:
        return None

    variant = image_name[index + len(marker) : -len(".img.xz")]
    return variant if variant else None


def fetch_release(index_url: str, release_name: str) -> dict[str, object]:
    with urllib.request.urlopen(index_url) as response:
        payload = json.load(response)

    releases = payload.get("releases")
    if not isinstance(releases, list):
        raise ValueError("index.json is missing releases")

    for release in releases:
        if isinstance(release, dict) and release.get("name") == release_name:
            return release
    raise ValueError(f"release {release_name!r} not found in {index_url}")


def collect_templates(templates_dir: Path) -> dict[str, Path]:
    templates: dict[str, Path] = {}
    for path in sorted(templates_dir.glob("*.yaml")):
        if path.name.startswith("."):
            continue
        device = path.stem
        templates[device] = path
    if not templates:
        raise ValueError(f"no device templates found in {templates_dir}")
    return templates


def select_rootfs_images(
    release: dict[str, object],
    pmos_device: str,
) -> list[RootfsSelection]:
    release_devices = release.get("devices")
    if not isinstance(release_devices, list):
        raise ValueError("release is missing devices")

    device_entry: dict[str, object] | None = None
    for item in release_devices:
        if isinstance(item, dict) and item.get("name") == pmos_device:
            device_entry = item
            break
    if device_entry is None:
        raise ValueError(f"device {pmos_device!r} not found in release")

    interfaces = device_entry.get("interfaces")
    if not isinstance(interfaces, list):
        raise ValueError(f"device {pmos_device!r} has no interfaces list")

    grouped: dict[tuple[str, str], list[dict[str, object]]] = {}
    for interface in interfaces:
        if not isinstance(interface, dict):
            continue
        ui_name = interface.get("name")
        images = interface.get("images")
        if not isinstance(ui_name, str) or not ui_name:
            continue
        if not isinstance(images, list):
            continue

        for image in images:
            if not isinstance(image, dict):
                continue
            image_name = image.get("name")
            image_url = image.get("url")
            timestamp = image.get("timestamp")
            image_sha512 = image.get("sha512")
            image_size = image.get("size")
            if not isinstance(image_name, str) or not isinstance(image_url, str):
                continue
            if not isinstance(timestamp, str):
                continue
            if not isinstance(image_sha512, str) or len(image_sha512) != 128:
                raise ValueError(f"image {image_name!r} is missing sha512")
            if not isinstance(image_size, int) or image_size <= 0:
                raise ValueError(f"image {image_name!r} has invalid size")

            variant_key = rootfs_variant(image_name, pmos_device)
            if variant_key is None:
                continue
            grouped.setdefault((ui_name, variant_key), []).append(image)

    selections: list[RootfsSelection] = []
    for (ui_name, variant_key), options in grouped.items():
        latest = max(options, key=lambda item: str(item.get("timestamp", "")))
        image_name = latest["name"]
        image_url = latest["url"]
        image_sha512 = latest["sha512"]
        image_size = latest["size"]
        timestamp = latest["timestamp"]
        if not isinstance(image_name, str) or not isinstance(image_url, str):
            continue
        if not isinstance(image_sha512, str) or len(image_sha512) != 128:
            raise ValueError(f"image {image_name!r} is missing sha512")
        if not isinstance(image_size, int) or image_size <= 0:
            raise ValueError(f"image {image_name!r} has invalid size")
        if not isinstance(timestamp, str):
            raise ValueError(f"image {image_name!r} is missing timestamp")
        selections.append(
            RootfsSelection(
                pmos_device=pmos_device,
                ui_name=ui_name,
                variant=variant_key or None,
                image_name=image_name,
                image_url=image_url,
                image_sha512=image_sha512,
                image_size=image_size,
                timestamp=timestamp,
            )
        )

    selections.sort(key=lambda item: (item.pmos_device, item.ui_name, item.variant or ""))
    if not selections:
        raise ValueError(f"no usable rootfs images found for {pmos_device!r}")
    return selections


def ensure_artifact_cached(
    image_url: str,
    image_sha512: str,
    image_size: int,
    cache_dir: Path,
) -> Path:
    parsed = urlparse(image_url)
    suffix = "".join(Path(parsed.path).suffixes)
    filename = image_sha512 if not suffix else f"{image_sha512}{suffix}"
    output_path = cache_dir / filename

    if output_path.exists():
        if output_path.stat().st_size == image_size:
            hasher = hashlib.sha512()
            with output_path.open("rb") as existing:
                while True:
                    chunk = existing.read(1024 * 1024)
                    if not chunk:
                        break
                    hasher.update(chunk)
            if hasher.hexdigest() == image_sha512:
                return output_path
        output_path.unlink()

    cache_dir.mkdir(parents=True, exist_ok=True)
    temp_path = output_path.with_suffix(output_path.suffix + ".tmp")
    hasher = hashlib.sha512()
    size = 0

    with urllib.request.urlopen(image_url) as response, temp_path.open("wb") as out:
        while True:
            chunk = response.read(1024 * 1024)
            if not chunk:
                break
            out.write(chunk)
            hasher.update(chunk)
            size += len(chunk)

    if size != image_size:
        temp_path.unlink(missing_ok=True)
        raise ValueError(
            f"downloaded artifact size mismatch for {image_url}: expected {image_size}, got {size}"
        )
    if hasher.hexdigest() != image_sha512:
        temp_path.unlink(missing_ok=True)
        raise ValueError(f"downloaded artifact digest mismatch for {image_url}")

    os.replace(temp_path, output_path)
    return output_path


def compile_manifest(
    fastboop: str,
    manifest_path: Path,
    output_path: Path,
    local_artifact: Path,
) -> None:
    subprocess.run(
        [
            fastboop,
            "bootprofile",
            "create",
            str(manifest_path),
            "-o",
            str(output_path),
            "--optimize",
            "--local-artifact",
            str(local_artifact),
        ],
        check=True,
    )


def render_manifest(
    template_text: str,
    release_name: str,
    selection: RootfsSelection,
) -> str:
    environment = Environment(
        autoescape=False,
        keep_trailing_newline=True,
        undefined=StrictUndefined,
    )
    template = environment.from_string(template_text)
    return template.render(
        release_name=release_name,
        pmos_device=selection.pmos_device,
        ui_name=selection.ui_name,
        variant=selection.variant,
        target_name=selection.target_name,
        image_name=selection.image_name,
        image_url=selection.image_url,
        image_sha512=selection.image_sha512,
        image_size=selection.image_size,
        timestamp=selection.timestamp,
    )


def ensure_bootpro(
    cache: CacheConfig,
    release_name: str,
    fastboop: str,
    fastboop_ver: str,
    manifest_content: str,
    selection: RootfsSelection,
    artifact_cache_dir: Path,
    bootpro_cache_dir: Path,
) -> Path:
    scope_hash = bootpro_scope_hash(manifest_content, fastboop_ver)
    output_path = bootpro_cache_dir / f"{selection.image_sha512}-{scope_hash}.bootpro"
    key = cache_key(cache.prefix, release_name, selection.image_sha512, scope_hash)

    if output_path.exists():
        return output_path

    if s3_head_object(cache, key):
        s3_get_object(cache, key, output_path)
        return output_path

    local_artifact = ensure_artifact_cached(
        image_url=selection.image_url,
        image_sha512=selection.image_sha512,
        image_size=selection.image_size,
        cache_dir=artifact_cache_dir,
    )

    with tempfile.TemporaryDirectory(prefix="bootpro-build-") as temp_dir:
        temp_dir_path = Path(temp_dir)
        manifest_path = temp_dir_path / "manifest.yaml"
        manifest_path.write_text(manifest_content, encoding="utf-8")
        compiled = temp_dir_path / "out.bootpro"
        compile_manifest(fastboop, manifest_path, compiled, local_artifact)

        output_path.parent.mkdir(parents=True, exist_ok=True)
        os.replace(compiled, output_path)
        s3_put_object(cache, key, output_path)

    return output_path


def write_channel(bootpro_paths: list[Path], output: Path) -> None:
    if not bootpro_paths:
        raise ValueError("no bootprofiles selected for channel")
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as channel:
        for bootpro_path in bootpro_paths:
            channel.write(bootpro_path.read_bytes())


def main() -> int:
    args = parse_args()
    templates_dir = Path(args.templates_dir)
    artifact_cache_dir = Path(args.artifact_cache_dir)
    bootpro_cache_dir = Path(args.bootpro_cache_dir)
    output = Path(args.output)

    cache = CacheConfig(
        bucket=args.cache_bucket,
        endpoint_url=args.cache_endpoint_url.strip() or None,
        prefix=normalize_prefix(args.cache_prefix),
    )

    try:
        templates = collect_templates(templates_dir)
        if args.only_device:
            only_device = args.only_device.strip()
            if only_device not in templates:
                raise ValueError(f"template not found for device {only_device!r}")
            selected_templates = {only_device: templates[only_device]}
        else:
            selected_templates = templates

        release = fetch_release(args.index_url, args.release)
        release_name = release.get("name")
        if not isinstance(release_name, str) or not release_name:
            raise ValueError("release is missing a name")
        fastboop_ver = fastboop_version(args.fastboop)

        selected_bootpros: list[Path] = []
        for pmos_device, template_path in sorted(selected_templates.items()):
            template_text = template_path.read_text(encoding="utf-8")
            selections = select_rootfs_images(release, pmos_device)
            for selection in selections:
                manifest_content = render_manifest(template_text, release_name, selection)
                bootpro = ensure_bootpro(
                    cache=cache,
                    release_name=release_name,
                    fastboop=args.fastboop,
                    fastboop_ver=fastboop_ver,
                    manifest_content=manifest_content,
                    selection=selection,
                    artifact_cache_dir=artifact_cache_dir,
                    bootpro_cache_dir=bootpro_cache_dir,
                )
                selected_bootpros.append(bootpro)

        write_channel(selected_bootpros, output)
    except Exception as err:
        print(f"error: {err}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
