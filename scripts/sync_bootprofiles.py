#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import shutil
import sys
import urllib.request
from collections.abc import Mapping, Sequence
from pathlib import Path

import yaml


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
) -> dict[str, str]:
    manifests: dict[str, str] = {}

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
            manifests[f"{file_stem}.yaml"] = render_manifest(
                profile_id=profile_id,
                display_name=display_name,
                image_url=latest["url"],
                image_sha512=sha512,
                image_size=size,
                device_profiles=device_profiles,
            )

    return manifests


def sync_bootprofiles(config: dict[str, list[str]], release: dict, out_dir: Path) -> None:
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

    configured_devices = set(config)
    for existing_dir in out_dir.iterdir():
        if existing_dir.is_dir() and existing_dir.name not in configured_devices:
            shutil.rmtree(existing_dir)

    for pmos_device in sorted(config):
        device_profiles = config[pmos_device]
        if pmos_device not in release_map:
            raise ValueError(f"allow-listed device {pmos_device!r} not found in release")

        manifests = build_manifests_for_device(
            release_name=release_name,
            device_entry=release_map[pmos_device],
            pmos_device=pmos_device,
            device_profiles=device_profiles,
        )
        if not manifests:
            raise ValueError(f"no rootfs images found for {pmos_device!r}")

        device_dir = out_dir / pmos_device
        device_dir.mkdir(parents=True, exist_ok=True)

        expected_paths = set()
        for filename, content in sorted(manifests.items()):
            file_path = device_dir / filename
            file_path.write_text(content, encoding="utf-8")
            expected_paths.add(file_path)

        for existing_file in device_dir.glob("*.yaml"):
            if existing_file not in expected_paths:
                existing_file.unlink()


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
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    config_path = Path(args.config)
    out_dir = Path(args.output_dir)

    try:
        config = load_config(config_path)
        release = fetch_release(args.index_url, args.release)
        sync_bootprofiles(config, release, out_dir)
    except Exception as err:
        print(f"error: {err}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
