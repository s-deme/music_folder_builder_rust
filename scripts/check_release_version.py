#!/usr/bin/env python3
"""Validate one SemVer across Cargo, Tauri, UI, tag, and prior release tags."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


SEMVER_PATTERN = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)


@dataclass(frozen=True)
class SemVer:
    text: str
    core: tuple[int, int, int]
    prerelease: tuple[str, ...] | None

    @classmethod
    def parse(cls, value: str) -> "SemVer":
        match = SEMVER_PATTERN.fullmatch(value)
        if not match:
            raise ValueError(f"not strict SemVer: {value}")
        major, minor, patch, prerelease, _build = match.groups()
        parts = tuple(prerelease.split(".")) if prerelease is not None else None
        if parts and any(part.isdigit() and len(part) > 1 and part.startswith("0") for part in parts):
            raise ValueError(f"numeric prerelease identifier has a leading zero: {value}")
        return cls(value, (int(major), int(minor), int(patch)), parts)


def compare(left: SemVer, right: SemVer) -> int:
    if left.core != right.core:
        return (left.core > right.core) - (left.core < right.core)
    if left.prerelease is None:
        return 0 if right.prerelease is None else 1
    if right.prerelease is None:
        return -1
    for left_part, right_part in zip(left.prerelease, right.prerelease):
        if left_part == right_part:
            continue
        left_numeric = left_part.isdigit()
        right_numeric = right_part.isdigit()
        if left_numeric and right_numeric:
            return (int(left_part) > int(right_part)) - (int(left_part) < int(right_part))
        if left_numeric != right_numeric:
            return -1 if left_numeric else 1
        return (left_part > right_part) - (left_part < right_part)
    return (len(left.prerelease) > len(right.prerelease)) - (
        len(left.prerelease) < len(right.prerelease)
    )


def repository_versions(root: Path) -> dict[str, str]:
    cargo = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    tauri = json.loads(
        (root / "crates" / "desktop" / "tauri.conf.json").read_text(encoding="utf-8")
    )
    ui = json.loads((root / "ui" / "package.json").read_text(encoding="utf-8"))
    return {
        "Cargo workspace": str(cargo["workspace"]["package"]["version"]),
        "Tauri bundle": str(tauri["version"]),
        "UI package": str(ui["version"]),
    }


def git_tags(root: Path) -> list[str]:
    output = subprocess.check_output(
        ["git", "tag", "--list", "v*"], cwd=root, text=True, encoding="utf-8"
    )
    return [line for line in output.splitlines() if line]


def listed_tags(path: Path) -> list[str]:
    tags: list[str] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        fields = line.split()
        if len(fields) != 2 or not fields[1].startswith("refs/tags/"):
            raise ValueError(f"invalid remote tag listing at line {line_number}")
        tag = fields[1].removeprefix("refs/tags/")
        if tag.endswith("^{}"):
            raise ValueError("remote tag listing must be produced with --refs")
        tags.append(tag)
    return tags


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[1]
    )
    parser.add_argument("--tag", help="require an exact v<repository-version> tag")
    parser.add_argument(
        "--tags-file",
        type=Path,
        help="use an immediate `git ls-remote --tags --refs` snapshot for monotonicity",
    )
    parser.add_argument(
        "--require-not-older",
        action="store_true",
        help="reject a version lower than any existing strict SemVer v* tag",
    )
    parser.add_argument(
        "--require-stable",
        action="store_true",
        help="reject prerelease versions for production signing/publishing",
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()

    try:
        versions = repository_versions(root)
        parsed = {name: SemVer.parse(value) for name, value in versions.items()}
    except (OSError, KeyError, json.JSONDecodeError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"release version check failed: {error}", file=sys.stderr)
        return 1

    distinct = {version.text for version in parsed.values()}
    if len(distinct) != 1:
        details = ", ".join(f"{name}={version.text}" for name, version in parsed.items())
        print(f"release version check failed: version mismatch: {details}", file=sys.stderr)
        return 1
    version = next(iter(parsed.values()))

    if args.require_stable and version.prerelease is not None:
        print(
            f"release version check failed: production version must be stable: {version.text}",
            file=sys.stderr,
        )
        return 1

    if args.tag and args.tag != f"v{version.text}":
        print(
            f"release version check failed: tag {args.tag!r} must equal v{version.text}",
            file=sys.stderr,
        )
        return 1

    if args.require_not_older:
        try:
            prior_versions = []
            tag_names = listed_tags(args.tags_file) if args.tags_file else git_tags(root)
            for tag in tag_names:
                try:
                    prior_versions.append((tag, SemVer.parse(tag[1:])))
                except ValueError:
                    continue
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            print(f"release version check failed: could not list tags: {error}", file=sys.stderr)
            return 1
        non_monotonic = [
            tag
            for tag, prior in prior_versions
            if tag != args.tag and compare(prior, version) >= 0
        ]
        if non_monotonic:
            print(
                "release version check failed: version is not newer than existing tag(s): "
                + ", ".join(sorted(non_monotonic)),
                file=sys.stderr,
            )
            return 1

    print(version.text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
