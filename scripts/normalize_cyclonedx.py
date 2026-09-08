#!/usr/bin/env python3
"""Normalize generator-specific volatile CycloneDX fields for one source commit."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
import uuid
from pathlib import Path
from typing import Any


COMMIT_PATTERN = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")


def canonicalize(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: canonicalize(value[key]) for key in sorted(value)}
    if isinstance(value, list):
        normalized = [canonicalize(item) for item in value]
        return sorted(
            normalized,
            key=lambda item: json.dumps(
                item, ensure_ascii=False, sort_keys=True, separators=(",", ":")
            ),
        )
    return value


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--epoch", type=int, required=True)
    parser.add_argument("--document-id", required=True)
    args = parser.parse_args(argv)

    commit = args.commit.lower()
    if not COMMIT_PATTERN.fullmatch(commit):
        parser.error("--commit must be a full lowercase hexadecimal commit ID")
    if args.epoch < 0:
        parser.error("--epoch must be non-negative")
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]*", args.document_id):
        parser.error("--document-id must be a stable lowercase identifier")

    try:
        document = json.loads(args.input.read_text(encoding="utf-8-sig"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"CycloneDX normalization failed: {error}", file=sys.stderr)
        return 1
    if not isinstance(document, dict) or document.get("bomFormat") != "CycloneDX":
        print("CycloneDX normalization failed: input is not a CycloneDX JSON object", file=sys.stderr)
        return 1

    serial = uuid.uuid5(
        uuid.NAMESPACE_URL,
        f"https://music-folder-builder.invalid/sbom/{args.document_id}/{commit}",
    )
    timestamp = dt.datetime.fromtimestamp(args.epoch, tz=dt.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    document["serialNumber"] = f"urn:uuid:{serial}"
    metadata = document.setdefault("metadata", {})
    if not isinstance(metadata, dict):
        print("CycloneDX normalization failed: metadata must be an object", file=sys.stderr)
        return 1
    metadata["timestamp"] = timestamp
    properties = document.setdefault("properties", [])
    if not isinstance(properties, list):
        print("CycloneDX normalization failed: properties must be an array", file=sys.stderr)
        return 1
    controlled_names = {
        "music-folder-builder:source-commit",
        "music-folder-builder:document-id",
    }
    properties[:] = [
        item
        for item in properties
        if not isinstance(item, dict) or item.get("name") not in controlled_names
    ]
    properties.extend(
        (
            {"name": "music-folder-builder:source-commit", "value": commit},
            {"name": "music-folder-builder:document-id", "value": args.document_id},
        )
    )
    normalized = canonicalize(document)
    try:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(
            json.dumps(normalized, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
            newline="\n",
        )
    except OSError as error:
        print(f"CycloneDX normalization failed: {error}", file=sys.stderr)
        return 1
    print(f"CycloneDX normalized: {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
