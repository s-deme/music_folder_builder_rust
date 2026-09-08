#!/usr/bin/env python3
"""Fail unless the checkout, requested release commit, and optional tag are identical."""

from __future__ import annotations

import argparse
import subprocess
import sys


def git(*arguments: str) -> str:
    return subprocess.check_output(
        ["git", *arguments], text=True, encoding="utf-8"
    ).strip()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected", required=True)
    parser.add_argument("--tag")
    args = parser.parse_args(argv)

    head = git("rev-parse", "HEAD")
    expected = git("rev-parse", f"{args.expected}^{{commit}}")
    if head != expected:
        print(f"checkout mismatch: HEAD={head}, expected={expected}", file=sys.stderr)
        return 1
    if args.tag:
        tag_commit = git("rev-list", "-n", "1", args.tag)
        if tag_commit != head:
            print(f"tag mismatch: {args.tag}={tag_commit}, HEAD={head}", file=sys.stderr)
            return 1
    print(f"source commit verified: {head}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
