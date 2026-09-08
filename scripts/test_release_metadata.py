#!/usr/bin/env python3
"""Self-test release metadata success and tamper/omission/path-escape rejection."""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import tempfile
import tomllib
from pathlib import Path

import verify_release_metadata
import write_release_metadata


COMMIT = "a" * 40


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def environment(root: Path, role: str) -> dict[str, object]:
    version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))[
        "workspace"
    ]["package"]["version"]
    rust = tomllib.loads((root / "rust-toolchain.toml").read_text(encoding="utf-8"))[
        "toolchain"
    ]["channel"]
    return {
        "schema_version": 1,
        "role": role,
        "source_commit": COMMIT,
        "repository_version": version,
        "workflow_ref": "self-test",
        "runner": {
            "os": "self-test",
            "arch": "x86_64",
            "image_os": "self-test-os",
            "image_version": "1",
        },
        "toolchain": {"rustc": {"release": rust}},
        "locks": {
            "Cargo.lock": sha256(root / "Cargo.lock"),
            "ui/package-lock.json": sha256(root / "ui" / "package-lock.json"),
        },
    }


def create_release(root: Path, case: Path, channel: str = "internal-unsigned") -> Path:
    bundle = case / "bundle"
    inputs = case / "inputs"
    release = case / "release"
    bundle.mkdir(parents=True)
    inputs.mkdir()
    (bundle / "music-folder.msi").write_bytes(b"synthetic-msi")
    (bundle / "music-folder-setup.exe").write_bytes(b"synthetic-nsis")
    (inputs / "rust.cdx.json").write_text('{"bomFormat":"CycloneDX"}\n', encoding="utf-8")
    (inputs / "cli-envelope.v1.schema.json").write_text("{}\n", encoding="utf-8")
    environment_paths = []
    roles = [
        ("windows-installer-build", "windows-build-environment.json"),
        ("sbom-generation", "sbom-environment.json"),
    ]
    if channel == "production-signed":
        roles.append(("windows-installer-package", "windows-package-environment.json"))
    for role, name in roles:
        path = inputs / name
        path.write_text(
            json.dumps(environment(root, role), sort_keys=True) + "\n", encoding="utf-8"
        )
        environment_paths.append(path)
    version = environment(root, "sbom-generation")["repository_version"]
    arguments = [
        "--bundle-dir",
        str(bundle),
        "--output-dir",
        str(release),
        "--commit",
        COMMIT,
        "--version",
        str(version),
        "--channel",
        channel,
        "--extra-file",
        str(inputs / "rust.cdx.json"),
        "--extra-file",
        str(inputs / "cli-envelope.v1.schema.json"),
    ]
    for path in environment_paths:
        arguments.extend(("--environment-file", str(path)))
    assert write_release_metadata.main(arguments) == 0
    return release


def verify(root: Path, release: Path, channel: str = "internal-unsigned") -> int:
    return verify_release_metadata.main(
        [
            "--release-dir",
            str(release),
            "--commit",
            COMMIT,
            "--channel",
            channel,
            "--repository-root",
            str(root),
        ]
    )


def rewrite_main_checksum(release: Path, name: str) -> None:
    checksum = release / "SHA256SUMS"
    lines = []
    for line in checksum.read_text(encoding="ascii").splitlines():
        _digest, current = line.split(" *", 1)
        lines.append(f"{sha256(release / current) if current == name else _digest} *{current}")
    checksum.write_text("\n".join(lines) + "\n", encoding="ascii", newline="\n")


def expect_rejected(root: Path, release: Path, label: str) -> None:
    with contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
        result = verify(root, release)
    if result == 0:
        raise AssertionError(f"negative release metadata case unexpectedly passed: {label}")


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix="music-folder-release-self-test-") as temporary:
        temporary_root = Path(temporary)

        positive = create_release(root, temporary_root / "positive")
        assert verify(root, positive) == 0
        sigstore = positive / "provenance.sigstore.json"
        sigstore.write_text('{"mediaType":"application/vnd.dev.sigstore.bundle+json;version=0.3"}\n', encoding="utf-8")
        with (positive / "SHA256SUMS").open("a", encoding="ascii", newline="\n") as stream:
            stream.write(f"{sha256(sigstore)} *{sigstore.name}\n")
        assert b"\r" not in (positive / "SHA256SUMS").read_bytes()
        assert verify(root, positive) == 0

        production = create_release(
            root, temporary_root / "production", channel="production-signed"
        )
        assert verify(root, production, channel="production-signed") == 0

        tampered = create_release(root, temporary_root / "tampered")
        (tampered / "music-folder.msi").write_bytes(b"tampered")
        expect_rejected(root, tampered, "installer digest tamper")

        omitted = create_release(root, temporary_root / "omitted")
        (omitted / "cli-envelope.v1.schema.json").unlink()
        expect_rejected(root, omitted, "omitted payload")

        checksum_omission = create_release(root, temporary_root / "checksum-omission")
        checksum = checksum_omission / "SHA256SUMS"
        checksum.write_text(
            "\n".join(
                line
                for line in checksum.read_text(encoding="ascii").splitlines()
                if not line.endswith("*cli-envelope.v1.schema.json")
            )
            + "\n",
            encoding="ascii",
            newline="\n",
        )
        expect_rejected(root, checksum_omission, "checksum omission")

        escaped = create_release(root, temporary_root / "escaped")
        manifest_path = escaped / "release-manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["artifacts"][0]["name"] = "../escape.msi"
        manifest_path.write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        rewrite_main_checksum(escaped, "release-manifest.json")
        expect_rejected(root, escaped, "manifest path escape")

    print("release metadata self-test passed (positive + tamper/omission/escape negatives)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
