#!/usr/bin/env python3

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Compile BootProfile manifests and concatenate a channel file."
    )
    parser.add_argument(
        "--manifests-dir",
        default="bootprofiles",
        help="Directory containing BootProfile YAML manifests",
    )
    parser.add_argument(
        "--compiled-dir",
        default="build/bootprofiles",
        help="Directory to place compiled .bootpro files",
    )
    parser.add_argument(
        "--output",
        default="build/channel/edge.bootchannel",
        help="Output channel path",
    )
    parser.add_argument(
        "--fastboop",
        default="fastboop",
        help="Path to the fastboop CLI binary",
    )
    return parser.parse_args()


def compile_manifests(
    fastboop: str, manifests_dir: Path, compiled_dir: Path
) -> list[tuple[Path, Path]]:
    manifest_paths = sorted(manifests_dir.rglob("*.yaml"))
    if not manifest_paths:
        raise ValueError(f"no manifests found under {manifests_dir}")

    if compiled_dir.exists():
        shutil.rmtree(compiled_dir)
    compiled_dir.mkdir(parents=True, exist_ok=True)

    compiled_pairs: list[tuple[Path, Path]] = []
    for manifest in manifest_paths:
        relative = manifest.relative_to(manifests_dir)
        output_file = (compiled_dir / relative).with_suffix(".bootpro")
        output_file.parent.mkdir(parents=True, exist_ok=True)

        subprocess.run(
            [fastboop, "bootprofile", "create", str(manifest), "-o", str(output_file)],
            check=True,
        )
        compiled_pairs.append((manifest, output_file))

    return compiled_pairs


def write_channel(compiled_pairs: list[tuple[Path, Path]], output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as channel:
        for _, bootpro in compiled_pairs:
            channel.write(bootpro.read_bytes())


def main() -> int:
    args = parse_args()
    manifests_dir = Path(args.manifests_dir)
    compiled_dir = Path(args.compiled_dir)
    output = Path(args.output)

    try:
        compiled_pairs = compile_manifests(args.fastboop, manifests_dir, compiled_dir)
        write_channel(compiled_pairs, output)
    except Exception as err:
        print(f"error: {err}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
