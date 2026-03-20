#!/usr/bin/env python3

from __future__ import annotations

import argparse
import shutil
import subprocess
import tempfile
from pathlib import Path


def run(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, check=check, text=True, capture_output=True)


def changed_devices(bootprofiles_dir: str) -> list[str]:
    diff = run("git", "diff", "--name-only", "--", bootprofiles_dir)
    devices: set[str] = set()
    for line in diff.stdout.splitlines():
        parts = Path(line).parts
        if len(parts) >= 2 and parts[0] == bootprofiles_dir:
            devices.add(parts[1])
    return sorted(devices)


def remote_branch_exists(branch: str) -> bool:
    probe = run("git", "ls-remote", "--heads", "origin", branch)
    return bool(probe.stdout.strip())


def open_pr_exists(base_branch: str, head_branch: str) -> bool:
    probe = run(
        "gh",
        "pr",
        "list",
        "--state",
        "open",
        "--base",
        base_branch,
        "--head",
        head_branch,
        "--json",
        "number",
        "--jq",
        ".[0].number",
    )
    return bool(probe.stdout.strip())


def copy_device_snapshot(snapshot_root: Path, target_root: Path, device: str) -> None:
    target_device_dir = target_root / device
    if target_device_dir.exists():
        shutil.rmtree(target_device_dir)

    source_device_dir = snapshot_root / device
    if source_device_dir.exists():
        shutil.copytree(source_device_dir, target_device_dir)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Open one PR per changed bootprofiles/<device> subtree."
    )
    parser.add_argument("--bootprofiles-dir", default="bootprofiles")
    parser.add_argument("--base-branch", default="main")
    parser.add_argument("--branch-prefix", default="automation/pmos-sync")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    devices = changed_devices(args.bootprofiles_dir)
    if not devices:
        print("no bootprofile changes detected")
        return 0

    bootprofiles_root = Path(args.bootprofiles_dir)
    with tempfile.TemporaryDirectory(prefix="bootprofiles-snapshot-") as temp_dir:
        snapshot_root = Path(temp_dir) / args.bootprofiles_dir
        if bootprofiles_root.exists():
            shutil.copytree(bootprofiles_root, snapshot_root)
        else:
            snapshot_root.mkdir(parents=True, exist_ok=True)

        for device in devices:
            branch = f"{args.branch_prefix}-{device}"
            if remote_branch_exists(branch):
                run("git", "checkout", "-B", branch, f"origin/{branch}")
            else:
                run("git", "checkout", "-B", branch, f"origin/{args.base_branch}")

            copy_device_snapshot(snapshot_root, bootprofiles_root, device)

            run("git", "add", "-A", f"{args.bootprofiles_dir}/{device}")
            staged_diff = run("git", "diff", "--cached", "--name-only", "--")
            if not staged_diff.stdout.strip():
                print(f"{device}: no staged changes on {branch}, skipping")
                continue

            commit_msg = f"chore(bootprofiles): refresh {device} from postmarketOS edge"
            run("git", "commit", "-m", commit_msg)
            run("git", "push", "-u", "origin", branch)

            if open_pr_exists(args.base_branch, branch):
                print(f"{device}: updated existing PR branch {branch}")
                continue

            title = f"pmos: refresh {device} bootprofiles"
            body = "\n".join(
                [
                    "## Summary",
                    f"- Refresh canonical BootProfile manifests for `{device}` from postmarketOS edge.",
                    "- Includes all available UIs for this device using latest rootfs images from index.json.",
                    "- Commits optimized `.bootpro` binaries alongside YAML manifests.",
                ]
            )
            run(
                "gh",
                "pr",
                "create",
                "--base",
                args.base_branch,
                "--head",
                branch,
                "--title",
                title,
                "--body",
                body,
            )
            print(f"{device}: opened PR from {branch}")

    run("git", "checkout", args.base_branch)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
