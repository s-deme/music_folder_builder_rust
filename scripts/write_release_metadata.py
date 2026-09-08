#!/usr/bin/env python3
"""Collect immutable Windows release inputs and emit deterministic SHA-256 metadata."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
from pathlib import Path


SHA_PATTERN = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def copy_unique(source: Path, output: Path) -> Path:
    destination = output / source.name
    if destination.exists():
        raise ValueError(f"refusing duplicate or stale release file: {destination.name}")
    shutil.copy2(source, destination)
    return destination


def collect_installers(bundle: Path) -> list[Path]:
    installers = sorted(
        path
        for path in bundle.rglob("*")
        if path.is_file()
        and (path.suffix.lower() == ".msi" or path.name.lower().endswith("-setup.exe"))
    )
    if not any(path.suffix.lower() == ".msi" for path in installers):
        raise ValueError(f"no MSI installer found below {bundle}")
    if not any(path.name.lower().endswith("-setup.exe") for path in installers):
        raise ValueError(f"no NSIS *-setup.exe installer found below {bundle}")
    return installers


def checksum_lines(paths: list[Path]) -> str:
    return "".join(f"{sha256(path)} *{path.name}\n" for path in sorted(paths))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--extra-file", action="append", type=Path, default=[])
    parser.add_argument("--environment-file", action="append", type=Path, default=[])
    parser.add_argument("--commit", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument(
        "--channel", choices=("internal-unsigned", "production-signed"), required=True
    )
    args = parser.parse_args(argv)

    commit = args.commit.lower()
    if not SHA_PATTERN.fullmatch(commit):
        parser.error("--commit must be a full 40- or 64-character lowercase hexadecimal ID")

    bundle = args.bundle_dir.resolve()
    output = args.output_dir.resolve()
    if not bundle.is_dir():
        parser.error(f"bundle directory does not exist: {bundle}")
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        parser.error(f"output directory must be empty: {output}")

    try:
        installer_sources = collect_installers(bundle)
        installers = [copy_unique(path, output) for path in installer_sources]
        extras = []
        for path in args.extra_file:
            source = path.resolve()
            if not source.is_file():
                raise ValueError(f"extra release file does not exist: {source}")
            extras.append(copy_unique(source, output))
        environment_files = []
        environments = []
        for path in args.environment_file:
            source = path.resolve()
            if not source.is_file():
                raise ValueError(f"environment file does not exist: {source}")
            environment = json.loads(source.read_text(encoding="utf-8"))
            if environment.get("schema_version") != 1:
                raise ValueError(f"unsupported environment schema: {source}")
            if environment.get("source_commit") != commit:
                raise ValueError(f"environment source commit differs: {source}")
            if environment.get("repository_version") != args.version:
                raise ValueError(f"environment repository version differs: {source}")
            environment_files.append(copy_unique(source, output))
            environments.append(environment)
    except (ValueError, json.JSONDecodeError) as error:
        print(f"release metadata error: {error}", file=sys.stderr)
        return 1

    payload_files = installers + extras + environment_files
    manifest = {
        "schema_version": 1,
        "product": "Music Folder Builder",
        "version": args.version,
        "source_commit": commit,
        "channel": args.channel,
        "signing": {
            "required": args.channel == "production-signed",
            "test_certificate": False,
        },
        "environments": sorted(environments, key=lambda item: str(item.get("role", ""))),
        "artifacts": [
            {
                "name": path.name,
                "kind": (
                    "msi"
                    if path.suffix.lower() == ".msi"
                    else "nsis"
                    if path.name.lower().endswith("-setup.exe")
                    else "sbom"
                    if path.name.lower().endswith(".cdx.json")
                    else "cli-schema"
                    if path.name.lower().endswith(".schema.json")
                    else "build-environment"
                    if path.name.lower().endswith("environment.json")
                    else "metadata"
                ),
                "size": path.stat().st_size,
                "sha256": sha256(path),
            }
            for path in sorted(payload_files)
        ],
    }
    manifest_path = output / "release-manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )

    installer_checksums = output / "INSTALLER-SHA256SUMS"
    installer_checksums.write_text(
        checksum_lines(installers), encoding="ascii", newline="\n"
    )
    all_payload = payload_files + [manifest_path, installer_checksums]
    (output / "SHA256SUMS").write_text(
        checksum_lines(all_payload), encoding="ascii", newline="\n"
    )
    print(f"release metadata written for {len(installers)} installer(s): {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
