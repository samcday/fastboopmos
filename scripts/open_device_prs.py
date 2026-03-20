#!/usr/bin/env python3

from __future__ import annotations

import argparse
import shlex
import shutil
import subprocess
import tempfile
import time
from pathlib import Path


def format_cmd(args: tuple[str, ...]) -> str:
    return " ".join(shlex.quote(arg) for arg in args)


def run(
    *args: str,
    check: bool = True,
    capture_output: bool = True,
) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        args,
        check=False,
        text=True,
        capture_output=capture_output,
    )
    if check and completed.returncode != 0:
        cmd = format_cmd(args)
        print(f"command failed ({completed.returncode}): {cmd}")
        if capture_output:
            if completed.stdout:
                print("stdout:")
                print(completed.stdout.rstrip())
            if completed.stderr:
                print("stderr:")
                print(completed.stderr.rstrip())
        raise subprocess.CalledProcessError(
            completed.returncode,
            args,
            output=completed.stdout,
            stderr=completed.stderr,
        )
    return completed


def run_with_retry(
    *args: str,
    attempts: int,
    delay_seconds: float,
) -> subprocess.CompletedProcess[str]:
    if attempts < 1:
        raise ValueError("attempts must be >= 1")

    last_err: subprocess.CalledProcessError | None = None
    for attempt in range(1, attempts + 1):
        try:
            return run(*args)
        except subprocess.CalledProcessError as err:
            last_err = err
            if attempt == attempts:
                break
            print(
                f"retrying after failed attempt {attempt}/{attempts}: {format_cmd(args)}"
            )
            time.sleep(delay_seconds)

    assert last_err is not None
    raise last_err


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
    parser.add_argument("--pr-create-attempts", type=int, default=3)
    parser.add_argument("--pr-create-retry-delay", type=float, default=3.0)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    devices = changed_devices(args.bootprofiles_dir)
    if not devices:
        print("no bootprofile changes detected")
        return 0

    bootprofiles_root = Path(args.bootprofiles_dir)
    failures: list[str] = []

    with tempfile.TemporaryDirectory(prefix="bootprofiles-snapshot-") as temp_dir:
        snapshot_root = Path(temp_dir) / args.bootprofiles_dir
        if bootprofiles_root.exists():
            shutil.copytree(bootprofiles_root, snapshot_root)
        else:
            snapshot_root.mkdir(parents=True, exist_ok=True)

        try:
            for device in devices:
                branch = f"{args.branch_prefix}-{device}"
                try:
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

                    commit_msg = (
                        f"chore(bootprofiles): refresh {device} from postmarketOS edge"
                    )
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
                    run_with_retry(
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
                        attempts=args.pr_create_attempts,
                        delay_seconds=args.pr_create_retry_delay,
                    )
                    print(f"{device}: opened PR from {branch}")
                except subprocess.CalledProcessError:
                    failures.append(device)
                    print(f"{device}: failed to update/open PR")
        finally:
            run("git", "checkout", args.base_branch)

    if failures:
        print(f"failed devices: {', '.join(failures)}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
