#!/usr/bin/env python3
"""Create a deterministic large audio corpus with hard links and copy fallback."""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path


AUDIO_SUFFIXES = {".flac", ".m4a", ".mp3", ".ogg"}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--count", type=int, default=100_000)
    args = parser.parse_args(argv)

    source = args.source.resolve()
    output = args.output.resolve()
    if not source.is_dir():
        parser.error(f"source fixture directory does not exist: {source}")
    if not 1 <= args.count <= 1_000_000:
        parser.error("--count must be within 1..1,000,000")
    fixture_roots = [source / suffix[1:] for suffix in sorted(AUDIO_SUFFIXES)]
    fixtures = sorted(
        path
        for fixture_root in fixture_roots
        if fixture_root.is_dir()
        for path in fixture_root.rglob("*")
        if path.is_file() and path.suffix.lower() in AUDIO_SUFFIXES
    )
    if not fixtures:
        parser.error(f"no supported audio fixtures found below {source}")
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        parser.error(f"output directory must be empty: {output}")

    copied = 0
    for index in range(args.count):
        fixture = fixtures[index % len(fixtures)]
        shard = output / f"shard-{index // 1000:04d}"
        shard.mkdir(exist_ok=True)
        destination = shard / f"item-{index:06d}{fixture.suffix.lower()}"
        try:
            os.link(fixture, destination)
        except OSError:
            shutil.copy2(fixture, destination)
            copied += 1

    materialized = sum(1 for path in output.rglob("*") if path.is_file())
    if materialized != args.count:
        print(
            f"benchmark corpus count mismatch: expected={args.count}, actual={materialized}",
            file=sys.stderr,
        )
        return 1
    print(
        f"benchmark corpus ready: items={materialized}, hard_links={materialized - copied}, copies={copied}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
