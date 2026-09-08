#!/usr/bin/env python3
"""Self-test positive, additive-minor, and command-payload CLI schema cases."""

from __future__ import annotations

import json
from pathlib import Path

from check_cli_schema import validate_against_schema


def envelope(command: str, data: object, *, status: str = "success", error: object = None) -> dict[str, object]:
    return {
        "schema_version": 1,
        "schema_revision": {"major": 1, "minor": 1},
        "command": command,
        "status": status,
        "result_type": command.replace(".", "_"),
        "correlation": {
            "logical_run_id": None,
            "attempt_id": None,
            "subject_id": None,
        },
        "counts": {},
        "diagnostics": [],
        "result": data,
        "data": data,
        "error": error,
    }


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    schema = json.loads(
        (root / "docs" / "release" / "cli-envelope.v1.schema.json").read_text(
            encoding="utf-8"
        )
    )
    recovery = envelope("recovery.list", [])
    assert not validate_against_schema(schema, recovery, schema)

    additive = json.loads(json.dumps(recovery))
    additive["future_minor_field"] = {"ignored": True}
    additive["correlation"]["future_id"] = None  # type: ignore[index]
    assert not validate_against_schema(schema, additive, schema)

    wrong_recovery = envelope("recovery.list", "lossy display path")
    assert validate_against_schema(schema, wrong_recovery, schema)
    incomplete_benchmark = envelope("benchmark", {"benchmark_schema_version": 2})
    assert validate_against_schema(schema, incomplete_benchmark, schema)

    failure = envelope(
        "apply",
        None,
        status="lease_busy",
        error={"code": "mutation_lease_busy", "message": "busy"},
    )
    failure["diagnostics"] = [
        {
            "code": "mutation_lease_busy",
            "severity": "warning",
            "phase": "apply",
            "message_key": "mutation_lease_busy",
            "correlation_id": None,
            "context": {},
        }
    ]
    assert not validate_against_schema(schema, failure, schema)
    print("CLI schema self-test passed (payload negatives + additive minor + failure)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
