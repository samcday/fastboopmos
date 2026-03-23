#!/usr/bin/env python3

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlparse

import yaml


class ManifestBuild:
    def __init__(
        self,
        content: str,
        image_url: str,
        image_sha512: str,
        image_size: int,
    ) -> None:
        self.content = content
        self.image_url = image_url
        self.image_sha512 = image_sha512
        self.image_size = image_size


@dataclass(frozen=True)
class RootfsArtifact:
    pmos_device: str
    ui_name: str
    variant: str | None
    image_name: str
    image_url: str
    image_sha512: str
    image_size: int
    timestamp: str

    @property
    def file_stem(self) -> str:
        if self.variant is None:
            return self.ui_name
        return f"{self.ui_name}-{self.variant}"

    @property
    def target_name(self) -> str:
        if self.variant is None:
            return self.pmos_device
        return f"{self.pmos_device}-{self.variant}"


@dataclass(frozen=True)
class MirrorConfig:
    bucket: str
    endpoint_url: str | None
    prefix: str
    public_base_url: str


def normalize_object_prefix(prefix: str) -> str:
    stripped = prefix.strip("/")
    if not stripped:
        raise ValueError("mirror prefix must not be empty")
    return stripped


def normalize_public_base_url(url: str) -> str:
    cleaned = url.rstrip("/")
    parsed = urlparse(cleaned)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise ValueError("mirror public base URL must be an absolute http/https URL")
    return cleaned


def parse_mirror_config(args: argparse.Namespace) -> MirrorConfig | None:
    bucket = args.mirror_bucket.strip()
    if not bucket:
        return None

    prefix = normalize_object_prefix(args.mirror_prefix)
    public_base_url = normalize_public_base_url(args.mirror_public_base_url)
    endpoint_url = args.mirror_endpoint_url.strip() or None
    return MirrorConfig(
        bucket=bucket,
        endpoint_url=endpoint_url,
        prefix=prefix,
        public_base_url=public_base_url,
    )


def aws_base_args(mirror: MirrorConfig) -> list[str]:
    args = ["aws", "s3api"]
    if mirror.endpoint_url:
        args.extend(["--endpoint-url", mirror.endpoint_url])
    return args


def aws_s3api_json(mirror: MirrorConfig, command: list[str]) -> object:
    completed = subprocess.run(
        aws_base_args(mirror) + command,
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode != 0:
        stderr = completed.stderr.strip()
        raise RuntimeError(f"aws s3api {' '.join(command)} failed: {stderr}")
    if not completed.stdout.strip():
        return {}
    return json.loads(completed.stdout)


def s3_head_object(mirror: MirrorConfig, key: str) -> dict[str, object] | None:
    completed = subprocess.run(
        aws_base_args(mirror)
        + ["head-object", "--bucket", mirror.bucket, "--key", key],
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode == 0:
        payload = completed.stdout.strip()
        if not payload:
            return {}
        loaded = json.loads(payload)
        if not isinstance(loaded, dict):
            raise RuntimeError("unexpected head-object payload")
        return loaded

    stderr_lower = completed.stderr.lower()
    if "not found" in stderr_lower or "404" in stderr_lower:
        return None
    raise RuntimeError(
        f"aws s3api head-object --bucket {mirror.bucket} --key {key} failed: "
        f"{completed.stderr.strip()}"
    )


def s3_put_object(
    mirror: MirrorConfig,
    key: str,
    source_path: Path,
    metadata: Mapping[str, str],
) -> None:
    metadata_arg = ",".join(f"{k}={v}" for k, v in sorted(metadata.items()))
    command = [
        "put-object",
        "--bucket",
        mirror.bucket,
        "--key",
        key,
        "--body",
        str(source_path),
    ]
    if metadata_arg:
        command.extend(["--metadata", metadata_arg])
    aws_s3api_json(mirror, command)


def s3_list_keys(mirror: MirrorConfig, prefix: str) -> set[str]:
    keys: set[str] = set()
    token: str | None = None
    while True:
        command = [
            "list-objects-v2",
            "--bucket",
            mirror.bucket,
            "--prefix",
            prefix,
        ]
        if token:
            command.extend(["--continuation-token", token])
        payload = aws_s3api_json(mirror, command)
        if not isinstance(payload, dict):
            raise RuntimeError("unexpected list-objects-v2 payload")

        contents = payload.get("Contents", [])
        if isinstance(contents, list):
            for entry in contents:
                if isinstance(entry, Mapping):
                    key = entry.get("Key")
                    if isinstance(key, str):
                        keys.add(key)

        token_value = payload.get("NextContinuationToken")
        if isinstance(token_value, str) and token_value:
            token = token_value
        else:
            break
    return keys


def s3_delete_object(mirror: MirrorConfig, key: str) -> None:
    aws_s3api_json(
        mirror,
        ["delete-object", "--bucket", mirror.bucket, "--key", key],
    )


def suffix_from_url(image_url: str) -> str:
    parsed = urlparse(image_url)
    return "".join(Path(parsed.path).suffixes)


def mirror_rootfs_object_key(mirror_prefix: str, release_name: str, artifact: RootfsArtifact) -> str:
    suffix = suffix_from_url(artifact.image_url)
    if suffix:
        return f"{mirror_prefix}/{release_name}/rootfs/{artifact.image_sha512}{suffix}"
    return f"{mirror_prefix}/{release_name}/rootfs/{artifact.image_sha512}"


def mirror_bootpro_object_key(
    mirror_prefix: str,
    release_name: str,
    fastboop_version: str,
    manifest_content: str,
) -> str:
    digest = hashlib.sha256()
    digest.update(b"bootpro-v1\0")
    digest.update(fastboop_version.encode("utf-8"))
    digest.update(b"\0")
    digest.update(manifest_content.encode("utf-8"))
    return f"{mirror_prefix}/{release_name}/bootpro/{digest.hexdigest()}.bootpro"


def mirror_public_url(mirror: MirrorConfig, key: str) -> str:
    return f"{mirror.public_base_url}/{key}"


def ensure_rootfs_mirrored(
    mirror: MirrorConfig,
    release_name: str,
    artifact: RootfsArtifact,
    local_artifact_path: Path,
) -> str:
    key = mirror_rootfs_object_key(mirror.prefix, release_name, artifact)
    existing = s3_head_object(mirror, key)
    if existing is not None:
        size = existing.get("ContentLength")
        metadata = existing.get("Metadata")
        existing_sha512 = None
        if isinstance(metadata, Mapping):
            meta_sha = metadata.get("sha512")
            if isinstance(meta_sha, str):
                existing_sha512 = meta_sha
        if size == artifact.image_size and existing_sha512 == artifact.image_sha512:
            return key

    s3_put_object(
        mirror,
        key,
        local_artifact_path,
        {
            "sha512": artifact.image_sha512,
            "size_bytes": str(artifact.image_size),
            "source_url_b64": base64.urlsafe_b64encode(
                artifact.image_url.encode("utf-8")
            ).decode("ascii"),
            "source_name_b64": base64.urlsafe_b64encode(
                artifact.image_name.encode("utf-8")
            ).decode("ascii"),
        },
    )
    return key


def fastboop_version(fastboop: str) -> str:
    completed = subprocess.run(
        [fastboop, "--version"],
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise RuntimeError(f"failed to determine fastboop version from {fastboop}")
    line = completed.stdout.strip().splitlines()
    if not line:
        raise RuntimeError("fastboop --version returned empty output")
    return line[0].strip()


def load_config(path: Path) -> dict[str, list[str]]:
    raw = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(raw, Mapping):
        raise ValueError("devices config must be a mapping")

    out: dict[str, list[str]] = {}
    for pmos_device, value in raw.items():
        if not isinstance(pmos_device, str) or not pmos_device:
            raise ValueError(f"invalid device key {pmos_device!r}")

        profiles: list[str]
        if isinstance(value, str):
            profiles = [value]
        elif isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
            profiles = [item for item in value if isinstance(item, str) and item]
        elif isinstance(value, Mapping):
            device_profiles = value.get("device_profiles")
            if isinstance(device_profiles, str):
                profiles = [device_profiles]
            elif isinstance(device_profiles, Sequence) and not isinstance(
                device_profiles, (str, bytes)
            ):
                profiles = [
                    item for item in device_profiles if isinstance(item, str) and item
                ]
            else:
                raise ValueError(
                    f"device {pmos_device!r} must define device_profiles as string or list"
                )
        else:
            raise ValueError(
                f"device {pmos_device!r} config must be a string, list, or mapping"
            )

        if not profiles:
            raise ValueError(f"device {pmos_device!r} has no device_profiles")
        out[pmos_device] = sorted(set(profiles))

    return out


def fetch_release(index_url: str, release_name: str) -> dict:
    with urllib.request.urlopen(index_url) as response:
        payload = json.load(response)

    releases = payload.get("releases")
    if not isinstance(releases, list):
        raise ValueError("index.json is missing releases")

    for release in releases:
        if release.get("name") == release_name:
            return release

    raise ValueError(f"release {release_name!r} not found in {index_url}")


def rootfs_variant(image_name: str, pmos_device: str) -> str | None:
    if not image_name.endswith(".img.xz"):
        return None
    if image_name.endswith("-boot.img.xz") or image_name.endswith("-bootpart.img.xz"):
        return None

    bare_suffix = f"-{pmos_device}.img.xz"
    if image_name.endswith(bare_suffix):
        return ""

    variant_prefix = f"-{pmos_device}-"
    if variant_prefix not in image_name:
        return None

    start = image_name.rfind(variant_prefix)
    if start == -1:
        return None
    variant = image_name[start + len(variant_prefix) : -len(".img.xz")]
    return variant if variant else None


def render_manifest(
    profile_id: str,
    display_name: str,
    image_url: str,
    image_sha512: str,
    image_size: int,
    device_profiles: list[str],
) -> str:
    lines = [
        f"id: {profile_id}",
        f"display_name: {display_name}",
        "rootfs:",
        "  ext4:",
        "    gpt:",
        "      index: 1",
        "      android_sparseimg:",
        "        xz:",
        f"          http: {image_url}",
        "          content:",
        f"            digest: sha512:{image_sha512}",
        f"            size_bytes: {image_size}",
        "kernel:",
        "  path: /vmlinuz",
        "  fat:",
        "    gpt:",
        "      index: 0",
        "      android_sparseimg:",
        "        xz:",
        f"          http: {image_url}",
        "          content:",
        f"            digest: sha512:{image_sha512}",
        f"            size_bytes: {image_size}",
        "dtbs:",
        "  path: /dtbs",
        "  fat:",
        "    gpt:",
        "      index: 0",
        "      android_sparseimg:",
        "        xz:",
        f"          http: {image_url}",
        "          content:",
        f"            digest: sha512:{image_sha512}",
        f"            size_bytes: {image_size}",
        "stage0:",
        "  devices:",
    ]
    for profile in device_profiles:
        lines.append(f"    {profile}: {{}}")
    return "\n".join(lines) + "\n"


def build_manifests_for_device(
    release_name: str,
    device_entry: Mapping[str, object],
    pmos_device: str,
    device_profiles: list[str],
    image_url_overrides: Mapping[str, str] | None = None,
) -> dict[str, ManifestBuild]:
    manifests: dict[str, ManifestBuild] = {}

    interfaces = device_entry.get("interfaces")
    if not isinstance(interfaces, list):
        raise ValueError(f"device {pmos_device!r} has no interfaces list")

    for interface in sorted(interfaces, key=lambda item: item.get("name", "")):
        ui_name = interface.get("name")
        images = interface.get("images")
        if not isinstance(ui_name, str) or not ui_name:
            raise ValueError(f"device {pmos_device!r} has interface without name")
        if not isinstance(images, list):
            continue

        grouped: dict[str, list[dict]] = {}
        for image in images:
            name = image.get("name")
            timestamp = image.get("timestamp")
            url = image.get("url")
            if not isinstance(name, str) or not isinstance(timestamp, str) or not isinstance(
                url, str
            ):
                continue
            variant = rootfs_variant(name, pmos_device)
            if variant is None:
                continue
            key = variant
            grouped.setdefault(key, []).append(image)

            sha512 = image.get("sha512")
            size = image.get("size")
            if not isinstance(sha512, str) or len(sha512) != 128:
                raise ValueError(f"image {name!r} is missing sha512")
            if not isinstance(size, int) or size <= 0:
                raise ValueError(f"image {name!r} has invalid size")

        for variant_key, variant_images in grouped.items():
            latest = max(variant_images, key=lambda item: item["timestamp"])
            sha512 = latest.get("sha512")
            size = latest.get("size")
            if not isinstance(sha512, str) or len(sha512) != 128:
                raise ValueError(
                    f"image {latest.get('name', '<unknown>')!r} is missing sha512"
                )
            if not isinstance(size, int) or size <= 0:
                raise ValueError(
                    f"image {latest.get('name', '<unknown>')!r} has invalid size"
                )
            variant = variant_key or None
            target_name = pmos_device if variant is None else f"{pmos_device}-{variant}"
            profile_id = f"pmos-{release_name}-{ui_name}-{target_name}"
            display_name = f"postmarketOS {release_name} {ui_name} {target_name}"
            file_stem = ui_name if variant is None else f"{ui_name}-{variant}"
            image_url = latest["url"]
            if image_url_overrides is not None:
                image_url = image_url_overrides.get(image_url, image_url)
            manifests[f"{file_stem}.yaml"] = ManifestBuild(
                content=render_manifest(
                    profile_id=profile_id,
                    display_name=display_name,
                    image_url=image_url,
                    image_sha512=sha512,
                    image_size=size,
                    device_profiles=device_profiles,
                ),
                image_url=latest["url"],
                image_sha512=sha512,
                image_size=size,
            )

    return manifests


def collect_rootfs_artifacts_for_device(
    device_entry: Mapping[str, object],
    pmos_device: str,
) -> list[RootfsArtifact]:
    interfaces = device_entry.get("interfaces")
    if not isinstance(interfaces, list):
        raise ValueError(f"device {pmos_device!r} has no interfaces list")

    artifacts: list[RootfsArtifact] = []
    for interface in sorted(interfaces, key=lambda item: item.get("name", "")):
        ui_name = interface.get("name")
        images = interface.get("images")
        if not isinstance(ui_name, str) or not ui_name:
            raise ValueError(f"device {pmos_device!r} has interface without name")
        if not isinstance(images, list):
            continue

        for image in images:
            name = image.get("name")
            timestamp = image.get("timestamp")
            url = image.get("url")
            sha512 = image.get("sha512")
            size = image.get("size")
            if not isinstance(name, str) or not isinstance(timestamp, str) or not isinstance(
                url, str
            ):
                continue

            variant = rootfs_variant(name, pmos_device)
            if variant is None:
                continue

            if not isinstance(sha512, str) or len(sha512) != 128:
                raise ValueError(f"image {name!r} is missing sha512")
            if not isinstance(size, int) or size <= 0:
                raise ValueError(f"image {name!r} has invalid size")

            artifacts.append(
                RootfsArtifact(
                    pmos_device=pmos_device,
                    ui_name=ui_name,
                    variant=variant if variant else None,
                    image_name=name,
                    image_url=url,
                    image_sha512=sha512,
                    image_size=size,
                    timestamp=timestamp,
                )
            )

    artifacts.sort(
        key=lambda item: (
            item.ui_name,
            item.variant or "",
            item.timestamp,
            item.image_name,
        )
    )
    return artifacts


def render_hint_manifest(
    release_name: str,
    artifact: RootfsArtifact,
    device_profiles: list[str],
    image_url: str,
) -> str:
    profile_id = (
        f"pmos-{release_name}-{artifact.ui_name}-{artifact.target_name}-{artifact.timestamp}"
    )
    display_name = (
        f"postmarketOS {release_name} {artifact.ui_name} {artifact.target_name} {artifact.timestamp}"
    )
    return render_manifest(
        profile_id=profile_id,
        display_name=display_name,
        image_url=image_url,
        image_sha512=artifact.image_sha512,
        image_size=artifact.image_size,
        device_profiles=device_profiles,
    )


def ensure_hint_mirrored(
    mirror: MirrorConfig,
    release_name: str,
    fastboop: str,
    fastboop_ver: str,
    artifact: RootfsArtifact,
    device_profiles: list[str],
    local_artifact_path: Path,
    image_url: str,
) -> str:
    manifest_content = render_hint_manifest(
        release_name=release_name,
        artifact=artifact,
        device_profiles=device_profiles,
        image_url=image_url,
    )
    key = mirror_bootpro_object_key(
        mirror_prefix=mirror.prefix,
        release_name=release_name,
        fastboop_version=fastboop_ver,
        manifest_content=manifest_content,
    )
    existing = s3_head_object(mirror, key)
    if existing is not None:
        return key

    with tempfile.TemporaryDirectory(prefix=".hint-build-") as temp_dir:
        temp_dir_path = Path(temp_dir)
        manifest_path = temp_dir_path / "hint.yaml"
        bootpro_path = temp_dir_path / "hint.bootpro"
        manifest_path.write_text(manifest_content, encoding="utf-8")
        compile_manifest(fastboop, manifest_path, bootpro_path, local_artifact_path)
        s3_put_object(
            mirror,
            key,
            bootpro_path,
            {
                "source_sha512": artifact.image_sha512,
                "fastboop_version_b64": base64.urlsafe_b64encode(
                    fastboop_ver.encode("utf-8")
                ).decode("ascii"),
            },
        )
    return key


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


def sync_bootprofiles(
    config: dict[str, list[str]],
    release: dict,
    out_dir: Path,
    compile_bootpro: bool,
    fastboop: str,
    artifact_cache_dir: Path,
    only_device: str | None,
    mirror: MirrorConfig | None,
    mirror_purge: bool,
) -> None:
    release_name = release.get("name")
    if not isinstance(release_name, str) or not release_name:
        raise ValueError("release is missing a name")

    release_devices = release.get("devices")
    if not isinstance(release_devices, list):
        raise ValueError("release is missing devices")

    release_map = {
        entry.get("name"): entry
        for entry in release_devices
        if isinstance(entry, Mapping) and isinstance(entry.get("name"), str)
    }

    out_dir.mkdir(parents=True, exist_ok=True)

    if only_device is not None:
        if only_device not in config:
            raise ValueError(f"device {only_device!r} is not in the allow-list config")
        target_devices = [only_device]
    else:
        target_devices = sorted(config)

    if only_device is None:
        configured_devices = set(config)
        for existing_dir in out_dir.iterdir():
            if existing_dir.is_dir() and existing_dir.name not in configured_devices:
                shutil.rmtree(existing_dir)

    desired_rootfs_keys: set[str] = set()
    desired_bootpro_keys: set[str] = set()
    image_url_overrides: dict[str, str] = {}
    fastboop_ver: str | None = None
    if compile_bootpro and mirror is not None:
        fastboop_ver = fastboop_version(fastboop)

    for pmos_device in target_devices:
        device_profiles = config[pmos_device]
        if pmos_device not in release_map:
            raise ValueError(f"allow-listed device {pmos_device!r} not found in release")

        artifacts = collect_rootfs_artifacts_for_device(
            device_entry=release_map[pmos_device],
            pmos_device=pmos_device,
        )
        if not artifacts:
            raise ValueError(f"no rootfs images found for {pmos_device!r}")

        if mirror is not None:
            for artifact in artifacts:
                local_artifact = ensure_artifact_cached(
                    image_url=artifact.image_url,
                    image_sha512=artifact.image_sha512,
                    image_size=artifact.image_size,
                    cache_dir=artifact_cache_dir,
                )
                rootfs_key = ensure_rootfs_mirrored(
                    mirror=mirror,
                    release_name=release_name,
                    artifact=artifact,
                    local_artifact_path=local_artifact,
                )
                desired_rootfs_keys.add(rootfs_key)
                mirrored_url = mirror_public_url(mirror, rootfs_key)
                image_url_overrides[artifact.image_url] = mirrored_url

                if compile_bootpro:
                    if fastboop_ver is None:
                        raise RuntimeError("fastboop version was not initialized")
                    bootpro_key = ensure_hint_mirrored(
                        mirror=mirror,
                        release_name=release_name,
                        fastboop=fastboop,
                        fastboop_ver=fastboop_ver,
                        artifact=artifact,
                        device_profiles=device_profiles,
                        local_artifact_path=local_artifact,
                        image_url=mirrored_url,
                    )
                    desired_bootpro_keys.add(bootpro_key)

        manifests = build_manifests_for_device(
            release_name=release_name,
            device_entry=release_map[pmos_device],
            pmos_device=pmos_device,
            device_profiles=device_profiles,
            image_url_overrides=image_url_overrides,
        )
        if not manifests:
            raise ValueError(f"no rootfs images found for {pmos_device!r}")

        device_dir = out_dir / pmos_device
        device_dir.mkdir(parents=True, exist_ok=True)

        expected_paths = set()
        expected_bootpro_paths = set()
        for filename, manifest in sorted(manifests.items()):
            file_path = device_dir / filename
            existing_manifest_content = (
                file_path.read_text(encoding="utf-8") if file_path.exists() else None
            )
            manifest_changed = existing_manifest_content != manifest.content
            if manifest_changed:
                file_path.write_text(manifest.content, encoding="utf-8")
            expected_paths.add(file_path)

            if compile_bootpro:
                bootpro_path = file_path.with_suffix(".bootpro")
                expected_bootpro_paths.add(bootpro_path)
                should_compile = manifest_changed or not bootpro_path.exists()
                if should_compile:
                    local_artifact = ensure_artifact_cached(
                        image_url=manifest.image_url,
                        image_sha512=manifest.image_sha512,
                        image_size=manifest.image_size,
                        cache_dir=artifact_cache_dir,
                    )
                    compile_manifest(fastboop, file_path, bootpro_path, local_artifact)

        for existing_file in device_dir.glob("*.yaml"):
            if existing_file not in expected_paths:
                existing_file.unlink()

        if compile_bootpro:
            for existing_file in device_dir.glob("*.bootpro"):
                if existing_file not in expected_bootpro_paths:
                    existing_file.unlink()

    if mirror is not None and mirror_purge:
        rootfs_prefix = f"{mirror.prefix}/{release_name}/rootfs/"
        existing_rootfs = s3_list_keys(mirror, rootfs_prefix)
        for key in sorted(existing_rootfs - desired_rootfs_keys):
            s3_delete_object(mirror, key)

        if compile_bootpro:
            bootpro_prefix = f"{mirror.prefix}/{release_name}/bootpro/"
            existing_bootpros = s3_list_keys(mirror, bootpro_prefix)
            for key in sorted(existing_bootpros - desired_bootpro_keys):
                s3_delete_object(mirror, key)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Sync canonical postmarketOS BootProfile YAMLs."
    )
    parser.add_argument(
        "--config",
        default="devices.yaml",
        help="Path to allow-list config",
    )
    parser.add_argument(
        "--output-dir",
        default="bootprofiles",
        help="Output directory for generated BootProfile manifests",
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
        "--compile-bootpro",
        action="store_true",
        help="Compile each generated manifest to optimized .bootpro",
    )
    parser.add_argument(
        "--fastboop",
        default="fastboop",
        help="Path to fastboop CLI binary",
    )
    parser.add_argument(
        "--artifact-cache-dir",
        default="build/pmos-artifacts",
        help="Cache directory for downloaded source artifacts",
    )
    parser.add_argument(
        "--only-device",
        help="Limit sync to one allow-listed postmarketOS device",
    )
    parser.add_argument(
        "--mirror-bucket",
        default="",
        help="S3-compatible bucket to mirror rootfs artifacts and optimized .bootpro hints",
    )
    parser.add_argument(
        "--mirror-endpoint-url",
        default="",
        help="Optional S3 endpoint URL (for B2/R2 and other S3-compatible backends)",
    )
    parser.add_argument(
        "--mirror-prefix",
        default="fastboopmos",
        help="Object prefix used for mirrored rootfs and .bootpro hint objects",
    )
    parser.add_argument(
        "--mirror-public-base-url",
        default="",
        help="Public base URL used in generated manifest URLs for mirrored rootfs objects",
    )
    parser.add_argument(
        "--mirror-purge",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="Delete mirrored rootfs/.bootpro objects not present in current index.json desired state",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    config_path = Path(args.config)
    out_dir = Path(args.output_dir)
    artifact_cache_dir = Path(args.artifact_cache_dir)
    mirror = parse_mirror_config(args)

    try:
        config = load_config(config_path)
        release = fetch_release(args.index_url, args.release)
        sync_bootprofiles(
            config,
            release,
            out_dir,
            compile_bootpro=args.compile_bootpro,
            fastboop=args.fastboop,
            artifact_cache_dir=artifact_cache_dir,
            only_device=args.only_device,
            mirror=mirror,
            mirror_purge=args.mirror_purge,
        )
    except Exception as err:
        print(f"error: {err}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
