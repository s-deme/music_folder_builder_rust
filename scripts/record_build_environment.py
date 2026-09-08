#!/usr/bin/env python3
"""Record exact release tools, lock digests, source identity, and runner image."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path


COMMIT_PATTERN = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def command(*arguments: str) -> str:
    executable = shutil.which(arguments[0]) or arguments[0]
    result = subprocess.run(
        [executable, *arguments[1:]],
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    return (result.stdout or result.stderr).strip()


def rustc_details(output: str) -> dict[str, str]:
    details: dict[str, str] = {}
    for line in output.splitlines():
        if ": " in line:
            key, value = line.split(": ", 1)
            details[key.replace("-", "_")] = value
    return details


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--role",
        choices=(
            "windows-installer-build",
            "windows-installer-package",
            "sbom-generation",
        ),
        required=True,
    )
    parser.add_argument("--commit", required=True)
    parser.add_argument("--workflow-ref", required=True)
    parser.add_argument("--expected-node-version", required=True)
    parser.add_argument("--expected-python-version", required=True)
    parser.add_argument("--tauri-cli-path")
    parser.add_argument("--expected-tauri-cli-version")
    parser.add_argument("--cargo-cyclonedx-path")
    parser.add_argument("--expected-cargo-cyclonedx-version")
    parser.add_argument("--require-runner-image", action="store_true")
    args = parser.parse_args(argv)

    root = args.root.resolve()
    commit = args.commit.lower()
    if not COMMIT_PATTERN.fullmatch(commit):
        parser.error("--commit must be a full lowercase hexadecimal commit ID")
    if bool(args.tauri_cli_path) != bool(args.expected_tauri_cli_version):
        parser.error("Tauri CLI path and expected version must be provided together")
    if bool(args.cargo_cyclonedx_path) != bool(args.expected_cargo_cyclonedx_version):
        parser.error("cargo-cyclonedx path and expected version must be provided together")

    try:
        workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        toolchain = tomllib.loads((root / "rust-toolchain.toml").read_text(encoding="utf-8"))
        repository_version = str(workspace["workspace"]["package"]["version"])
        expected_rust = str(toolchain["toolchain"]["channel"])
        rustc_output = command("rustc", "--version", "--verbose")
        rustc = rustc_details(rustc_output)
        cargo_version = command("cargo", "--version")
        node_version = command("node", "--version").removeprefix("v")
        npm_version = command("npm", "--version")
        python_version = ".".join(str(part) for part in sys.version_info[:3])
        optional_tools: dict[str, str] = {}
        if args.tauri_cli_path:
            optional_tools["tauri_cli"] = command(args.tauri_cli_path, "--version")
        if args.cargo_cyclonedx_path:
            optional_tools["cargo_cyclonedx"] = command(
                args.cargo_cyclonedx_path, "--version"
            )
    except (OSError, KeyError, subprocess.CalledProcessError, tomllib.TOMLDecodeError) as error:
        print(f"build environment recording failed: {error}", file=sys.stderr)
        return 1

    errors: list[str] = []
    if rustc.get("release") != expected_rust:
        errors.append(f"rustc release {rustc.get('release')} != pinned {expected_rust}")
    if node_version != args.expected_node_version:
        errors.append(f"Node {node_version} != pinned {args.expected_node_version}")
    if python_version != args.expected_python_version:
        errors.append(f"Python {python_version} != pinned {args.expected_python_version}")
    if args.expected_tauri_cli_version and args.expected_tauri_cli_version not in optional_tools.get(
        "tauri_cli", ""
    ):
        errors.append("Tauri CLI output does not contain its pinned version")
    if args.expected_cargo_cyclonedx_version and args.expected_cargo_cyclonedx_version not in optional_tools.get(
        "cargo_cyclonedx", ""
    ):
        errors.append("cargo-cyclonedx output does not contain its pinned version")

    runner = {
        "os": os.environ.get("RUNNER_OS", sys.platform),
        "arch": os.environ.get("RUNNER_ARCH", "unknown"),
        "image_os": os.environ.get("ImageOS", ""),
        "image_version": os.environ.get("ImageVersion", ""),
    }
    if args.require_runner_image and (not runner["image_os"] or not runner["image_version"]):
        errors.append("GitHub hosted runner ImageOS/ImageVersion are required")
    if errors:
        print("build environment recording failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    document = {
        "schema_version": 1,
        "role": args.role,
        "source_commit": commit,
        "repository_version": repository_version,
        "workflow_ref": args.workflow_ref,
        "runner": runner,
        "toolchain": {
            "rustc": rustc,
            "cargo": cargo_version,
            "node": node_version,
            "npm": npm_version,
            "python": python_version,
            **optional_tools,
        },
        "locks": {
            "Cargo.lock": sha256(root / "Cargo.lock"),
            "ui/package-lock.json": sha256(root / "ui" / "package-lock.json"),
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"build environment recorded: role={args.role}, output={args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
