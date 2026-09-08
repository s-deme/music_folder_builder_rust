#!/usr/bin/env python3
"""Verify release checksums, manifest identity, signing policy, and installer set."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tomllib
from pathlib import Path


CHECKSUM_PATTERN = re.compile(r"^([0-9a-f]{64}) [ *](.+)$")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def confined_release_path(root: Path, name: str) -> tuple[Path | None, str | None]:
    if not name or "\\" in name:
        return None, "name must be a non-empty canonical forward-slash path"
    candidate = (root / name).resolve()
    try:
        relative = candidate.relative_to(root.resolve()).as_posix()
    except ValueError:
        return None, "path escapes release directory"
    if relative != name:
        return None, "path is not canonical within release directory"
    return candidate, None


def parse_checksum_file(root: Path, checksum_file: Path) -> tuple[dict[str, str], list[str]]:
    entries: dict[str, str] = {}
    errors: list[str] = []
    for line_number, line in enumerate(checksum_file.read_text(encoding="ascii").splitlines(), 1):
        match = CHECKSUM_PATTERN.fullmatch(line)
        if not match:
            errors.append(f"{checksum_file.name}:{line_number}: malformed checksum line")
            continue
        expected, name = match.groups()
        if name in entries:
            errors.append(f"{checksum_file.name}:{line_number}: duplicate entry for {name}")
            continue
        candidate, path_error = confined_release_path(root, name)
        if path_error:
            errors.append(f"{checksum_file.name}:{line_number}: {path_error}: {name}")
            continue
        assert candidate is not None
        entries[name] = expected
        if not candidate.is_file():
            errors.append(f"{checksum_file.name}:{line_number}: missing {name}")
        elif sha256(candidate) != expected:
            errors.append(f"{checksum_file.name}:{line_number}: digest mismatch for {name}")
    if not entries:
        errors.append(f"{checksum_file.name}: checksum file must not be empty")
    return entries, errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-dir", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument(
        "--channel", choices=("internal-unsigned", "production-signed"), required=True
    )
    parser.add_argument("--repository-root", type=Path)
    args = parser.parse_args(argv)

    root = args.release_dir.resolve()
    errors: list[str] = []
    for name in ("SHA256SUMS", "INSTALLER-SHA256SUMS", "release-manifest.json"):
        if not (root / name).is_file():
            errors.append(f"missing required release metadata: {name}")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1

    all_checksums, checksum_errors = parse_checksum_file(root, root / "SHA256SUMS")
    installer_checksums, installer_checksum_errors = parse_checksum_file(
        root, root / "INSTALLER-SHA256SUMS"
    )
    errors.extend(checksum_errors)
    errors.extend(installer_checksum_errors)
    physical_files = {
        path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()
    }
    expected_checksum_files = physical_files - {"SHA256SUMS"}
    if set(all_checksums) != expected_checksum_files:
        errors.append(
            "SHA256SUMS must cover every release file exactly once: "
            f"missing={sorted(expected_checksum_files - set(all_checksums))}, "
            f"extra={sorted(set(all_checksums) - expected_checksum_files)}"
        )
    try:
        manifest = json.loads((root / "release-manifest.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"release metadata verification failed: invalid manifest: {error}", file=sys.stderr)
        return 1
    if manifest.get("schema_version") != 1:
        errors.append("release manifest schema_version must be 1")
    if manifest.get("source_commit") != args.commit.lower():
        errors.append("release manifest source_commit differs from checked-out commit")
    if manifest.get("channel") != args.channel:
        errors.append("release manifest channel differs from requested channel")
    signing_required = manifest.get("signing", {}).get("required")
    if signing_required != (args.channel == "production-signed"):
        errors.append("release manifest signing policy does not match channel")
    environments = manifest.get("environments", [])
    if not isinstance(environments, list):
        errors.append("release manifest environments must be an array")
        environments = []
    environment_roles = {
        environment.get("role")
        for environment in environments
        if isinstance(environment, dict)
    }
    expected_environment_roles = {"windows-installer-build", "sbom-generation"}
    if args.channel == "production-signed":
        expected_environment_roles.add("windows-installer-package")
    if environment_roles != expected_environment_roles:
        errors.append(
            "release manifest environment roles differ from channel policy: "
            f"expected={sorted(expected_environment_roles)}, actual={sorted(environment_roles)}"
        )
    for environment in environments:
        if not isinstance(environment, dict):
            errors.append("release environment entries must be objects")
            continue
        if environment.get("schema_version") != 1:
            errors.append("release environment schema_version must be 1")
        if environment.get("source_commit") != args.commit.lower():
            errors.append("release environment source_commit differs")
        locks = environment.get("locks", {})
        for lock_name in ("Cargo.lock", "ui/package-lock.json"):
            if not re.fullmatch(r"[0-9a-f]{64}", str(locks.get(lock_name, ""))):
                errors.append(f"release environment has invalid digest for {lock_name}")
        runner = environment.get("runner", {})
        if not runner.get("image_os") or not runner.get("image_version"):
            errors.append("release environment must record hosted runner image OS/version")

    if args.repository_root:
        repository = args.repository_root.resolve()
        expected_locks = {
            "Cargo.lock": sha256(repository / "Cargo.lock"),
            "ui/package-lock.json": sha256(repository / "ui" / "package-lock.json"),
        }
        expected_rust = tomllib.loads(
            (repository / "rust-toolchain.toml").read_text(encoding="utf-8")
        )["toolchain"]["channel"]
        for environment in environments:
            if not isinstance(environment, dict):
                continue
            if environment.get("locks") != expected_locks:
                errors.append(f"{environment.get('role')} lock digests differ from checkout")
            if environment.get("toolchain", {}).get("rustc", {}).get("release") != expected_rust:
                errors.append(f"{environment.get('role')} Rust toolchain differs from checkout")

    artifacts = manifest.get("artifacts", [])
    if not isinstance(artifacts, list):
        errors.append("release manifest artifacts must be an array")
        artifacts = []
    artifact_names: set[str] = set()
    duplicate_names: set[str] = set()
    kinds = {artifact.get("kind") for artifact in artifacts if isinstance(artifact, dict)}
    if not {"msi", "nsis"}.issubset(kinds):
        errors.append("release manifest must contain both MSI and NSIS installers")
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            errors.append("release manifest artifact entries must be objects")
            continue
        name = str(artifact.get("name", ""))
        if name in artifact_names:
            duplicate_names.add(name)
        artifact_names.add(name)
        path, path_error = confined_release_path(root, name)
        if path_error or "/" in name:
            errors.append(f"manifest artifact name is not a confined basename: {name}")
            continue
        assert path is not None
        if not path.is_file():
            errors.append(f"manifest artifact is missing: {path.name}")
        elif artifact.get("sha256") != sha256(path):
            errors.append(f"manifest digest mismatch: {path.name}")
        elif artifact.get("size") != path.stat().st_size:
            errors.append(f"manifest size mismatch: {path.name}")
        if all_checksums.get(name) != artifact.get("sha256"):
            errors.append(f"manifest and SHA256SUMS disagree: {name}")
    if duplicate_names:
        errors.append(f"release manifest contains duplicate artifact name(s): {sorted(duplicate_names)}")

    metadata_names = {"SHA256SUMS", "INSTALLER-SHA256SUMS", "release-manifest.json"}
    payload_files = {
        name
        for name in physical_files - metadata_names
        if not name.endswith(".sigstore.json")
    }
    if artifact_names != payload_files:
        errors.append(
            "manifest artifacts must exactly cover release payload files: "
            f"missing={sorted(payload_files - artifact_names)}, "
            f"extra={sorted(artifact_names - payload_files)}"
        )
    installer_names = {
        str(artifact.get("name"))
        for artifact in artifacts
        if isinstance(artifact, dict) and artifact.get("kind") in {"msi", "nsis"}
    }
    if set(installer_checksums) != installer_names:
        errors.append(
            "INSTALLER-SHA256SUMS must exactly cover manifest installers: "
            f"missing={sorted(installer_names - set(installer_checksums))}, "
            f"extra={sorted(set(installer_checksums) - installer_names)}"
        )
    for name in installer_names:
        artifact = next(
            item for item in artifacts if isinstance(item, dict) and item.get("name") == name
        )
        if installer_checksums.get(name) != artifact.get("sha256"):
            errors.append(f"manifest and INSTALLER-SHA256SUMS disagree: {name}")

    if errors:
        print("release metadata verification failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print(f"release metadata verified: {root}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
