#!/usr/bin/env python3

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Concatenate compiled .bootpro files into a channel file."
    )
    parser.add_argument(
        "--bootpros-dir",
        default="bootprofiles",
        help="Directory containing compiled .bootpro files",
    )
    parser.add_argument(
        "--output",
        default="build/channel/edge.channel",
        help="Output channel path",
    )
    return parser.parse_args()


def collect_bootpro_paths(bootpros_dir: Path) -> list[Path]:
    paths = sorted(bootpros_dir.rglob("*.bootpro"))
    if not paths:
        raise ValueError(f"no .bootpro files found under {bootpros_dir}")
    return paths


def write_channel(bootpro_paths: list[Path], output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as channel:
        for bootpro in bootpro_paths:
            channel.write(bootpro.read_bytes())


def main() -> int:
    args = parse_args()
    bootpros_dir = Path(args.bootpros_dir)
    output = Path(args.output)

    try:
        bootpro_paths = collect_bootpro_paths(bootpros_dir)
        write_channel(bootpro_paths, output)
    except Exception as err:
        print(f"error: {err}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
