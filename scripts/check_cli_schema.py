#!/usr/bin/env python3
"""Validate the published CLI 1.1 schema and one-line process samples."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


CORRELATION_FIELDS = {"logical_run_id", "attempt_id", "subject_id"}
DIAGNOSTIC_FIELDS = {
    "code",
    "severity",
    "phase",
    "message_key",
    "correlation_id",
    "context",
}
EXIT_CODES = {
    "success": 0,
    "usage": 2,
    "blocked": 3,
    "partial": 4,
    "internal": 5,
    "lease_busy": 6,
    "recovery_required": 7,
    "verify_mismatch": 8,
    "cancelled": 9,
}
STATUSES = {
    "success",
    "blocked",
    "partial",
    "error",
    "lease_busy",
    "recovery_required",
    "verify_mismatch",
    "cancelled",
}


def _json_type_matches(expected: str, value: object) -> bool:
    if expected == "null":
        return value is None
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "string":
        return isinstance(value, str)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    return False


def _resolve_ref(root: dict[str, Any], reference: str) -> object:
    if not reference.startswith("#/"):
        raise ValueError(f"only local JSON pointers are supported: {reference}")
    current: object = root
    for raw_part in reference[2:].split("/"):
        part = raw_part.replace("~1", "/").replace("~0", "~")
        if not isinstance(current, dict) or part not in current:
            raise ValueError(f"unresolved JSON pointer: {reference}")
        current = current[part]
    return current


def validate_against_schema(
    schema: object,
    instance: object,
    root: dict[str, Any],
    path: str = "$",
) -> list[str]:
    """Evaluate the draft-2020-12 subset used by the published contract."""
    if schema is True:
        return []
    if schema is False:
        return [f"{path}: value is forbidden"]
    if not isinstance(schema, dict):
        return [f"{path}: invalid schema node"]
    errors: list[str] = []
    if "$ref" in schema:
        try:
            resolved = _resolve_ref(root, str(schema["$ref"]))
        except ValueError as error:
            return [f"{path}: {error}"]
        errors.extend(validate_against_schema(resolved, instance, root, path))

    if "const" in schema and instance != schema["const"]:
        errors.append(f"{path}: expected const {schema['const']!r}")
    if "enum" in schema and instance not in schema["enum"]:
        errors.append(f"{path}: value is absent from enum")
    expected_types = schema.get("type")
    if expected_types is not None:
        type_names = [expected_types] if isinstance(expected_types, str) else expected_types
        if not isinstance(type_names, list) or not any(
            isinstance(name, str) and _json_type_matches(name, instance)
            for name in type_names
        ):
            return errors + [f"{path}: type differs from {type_names!r}"]

    if "oneOf" in schema:
        matches = [
            validate_against_schema(option, instance, root, path)
            for option in schema["oneOf"]
        ]
        if sum(not result for result in matches) != 1:
            errors.append(f"{path}: oneOf must match exactly once")
    for part in schema.get("allOf", []):
        errors.extend(validate_against_schema(part, instance, root, path))
    if "if" in schema and not validate_against_schema(schema["if"], instance, root, path):
        if "then" in schema:
            errors.extend(validate_against_schema(schema["then"], instance, root, path))
        elif "else" in schema:
            errors.extend(validate_against_schema(schema["else"], instance, root, path))

    if isinstance(instance, dict):
        required = schema.get("required", [])
        if isinstance(required, list):
            for name in required:
                if name not in instance:
                    errors.append(f"{path}: missing required property {name!r}")
        properties = schema.get("properties", {})
        if isinstance(properties, dict):
            for name, child_schema in properties.items():
                if name in instance:
                    errors.extend(
                        validate_against_schema(
                            child_schema, instance[name], root, f"{path}.{name}"
                        )
                    )
            extras = instance.keys() - properties.keys()
            additional = schema.get("additionalProperties", True)
            if additional is False and extras:
                errors.append(f"{path}: additional properties are forbidden: {sorted(extras)}")
            elif isinstance(additional, dict):
                for name in extras:
                    errors.extend(
                        validate_against_schema(
                            additional, instance[name], root, f"{path}.{name}"
                        )
                    )
    if isinstance(instance, list):
        if isinstance(schema.get("minItems"), int) and len(instance) < schema["minItems"]:
            errors.append(f"{path}: array has fewer than {schema['minItems']} items")
        if isinstance(schema.get("maxItems"), int) and len(instance) > schema["maxItems"]:
            errors.append(f"{path}: array has more than {schema['maxItems']} items")
        if "items" in schema:
            for index, value in enumerate(instance):
                errors.extend(
                    validate_against_schema(schema["items"], value, root, f"{path}[{index}]")
                )
    if isinstance(instance, str):
        if isinstance(schema.get("minLength"), int) and len(instance) < schema["minLength"]:
            errors.append(f"{path}: string is shorter than {schema['minLength']}")
        if isinstance(schema.get("pattern"), str) and not re.search(schema["pattern"], instance):
            errors.append(f"{path}: string does not match {schema['pattern']!r}")
    if isinstance(instance, (int, float)) and not isinstance(instance, bool):
        if isinstance(schema.get("minimum"), (int, float)) and instance < schema["minimum"]:
            errors.append(f"{path}: number is below {schema['minimum']}")
        if isinstance(schema.get("maximum"), (int, float)) and instance > schema["maximum"]:
            errors.append(f"{path}: number is above {schema['maximum']}")
    return errors


def validate_sample(schema: dict[str, Any], sample: dict[str, Any], label: str) -> list[str]:
    errors: list[str] = []
    prefix = f"{label}: "
    required = set(schema.get("required", []))
    missing = required - sample.keys()
    if missing:
        errors.append(prefix + f"missing required field(s): {sorted(missing)}")
    properties = schema.get("properties", {})
    if sample.get("schema_version") != properties.get("schema_version", {}).get("const"):
        errors.append(prefix + "schema_version differs from schema const")
    revision = sample.get("schema_revision")
    if revision != {"major": 1, "minor": 1}:
        errors.append(prefix + "schema_revision must be exactly {major:1,minor:1}")
    command = sample.get("command")
    if not isinstance(command, str) or not command:
        errors.append(prefix + "command must be a non-empty string")
    elif sample.get("result_type") != command.replace(".", "_"):
        errors.append(prefix + "result_type must be the command discriminator")
    if sample.get("status") not in properties.get("status", {}).get("enum", []):
        errors.append(prefix + "status is absent from schema enum")
    correlation = sample.get("correlation")
    if not isinstance(correlation, dict) or not CORRELATION_FIELDS.issubset(correlation):
        errors.append(prefix + "correlation must contain the three stable ID fields")
    elif any(value is not None and not isinstance(value, str) for value in correlation.values()):
        errors.append(prefix + "correlation IDs must be strings or null")
    counts = sample.get("counts")
    if not isinstance(counts, dict) or any(
        not isinstance(value, int) or isinstance(value, bool) or value < 0
        for value in counts.values()
    ):
        errors.append(prefix + "counts must be non-negative integer fields")
    diagnostics = sample.get("diagnostics")
    if not isinstance(diagnostics, list):
        errors.append(prefix + "diagnostics must be an array")
    else:
        for index, diagnostic in enumerate(diagnostics):
            if not isinstance(diagnostic, dict) or not DIAGNOSTIC_FIELDS.issubset(diagnostic):
                errors.append(prefix + f"diagnostics[{index}] has the wrong typed fields")
                continue
            if diagnostic.get("severity") not in {"info", "warning", "error"}:
                errors.append(prefix + f"diagnostics[{index}].severity is invalid")
            for field in ("code", "phase", "message_key"):
                if not isinstance(diagnostic.get(field), str) or not diagnostic[field]:
                    errors.append(prefix + f"diagnostics[{index}].{field} must be non-empty")
            if diagnostic.get("correlation_id") is not None and not isinstance(
                diagnostic["correlation_id"], str
            ):
                errors.append(prefix + f"diagnostics[{index}].correlation_id is invalid")
            if not isinstance(diagnostic.get("context"), dict):
                errors.append(prefix + f"diagnostics[{index}].context must be an object")
    if sample.get("result") != sample.get("data"):
        errors.append(prefix + "result compatibility alias must equal data")
    error = sample.get("error")
    if error is None:
        if diagnostics is not None and not isinstance(diagnostics, list):
            errors.append(prefix + "result envelope diagnostics is invalid")
    elif not (
        isinstance(error, dict)
        and {"code", "message"}.issubset(error)
        and isinstance(error.get("code"), str)
        and bool(error["code"])
        and isinstance(error.get("message"), str)
    ):
        errors.append(prefix + "error must be null or exactly typed {code,message}")
    elif sample.get("result") is not None or not diagnostics:
        errors.append(prefix + "failure envelope must have null result and typed diagnostic")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    root = Path(__file__).resolve().parents[1]
    parser.add_argument(
        "--schema",
        type=Path,
        default=root / "docs" / "release" / "cli-envelope.v1.schema.json",
    )
    parser.add_argument("--sample", action="append", type=Path, required=True)
    parser.add_argument(
        "--cli-source", type=Path, default=root / "crates" / "cli" / "src" / "output.rs"
    )
    args = parser.parse_args(argv)

    errors: list[str] = []
    try:
        schema = json.loads(args.schema.read_text(encoding="utf-8"))
        source = args.cli_source.read_text(encoding="utf-8")
    except (OSError, json.JSONDecodeError) as error:
        print(f"CLI schema check failed: {error}", file=sys.stderr)
        return 1

    if schema.get("$schema") != "https://json-schema.org/draft/2020-12/schema":
        errors.append("schema must identify JSON Schema draft 2020-12")
    if schema.get("type") != "object" or schema.get("additionalProperties") is not True:
        errors.append("envelope schema must allow additive minor fields")
    version = schema.get("properties", {}).get("schema_version", {}).get("const")
    revision = schema.get("$defs", {}).get("schemaRevision", {}).get("properties", {})
    major = revision.get("major", {}).get("const")
    minor = revision.get("minor", {}).get("const")
    if (version, major, minor) != (1, 1, 1):
        errors.append("published compatibility identity must be schema_version=1 revision=1.1")
    for field, expected in (("schema_version", 1), ("major", 1), ("minor", 1)):
        if not re.search(rf'"{field}"\s*:\s*{expected}\b', source):
            errors.append(f"CLI source does not emit published {field}={expected}")
    native_path = schema.get("$defs", {}).get("nativePath", {})
    if set(native_path.get("required", [])) != {
        "schema_version",
        "role",
        "display",
        "encoding",
        "raw_base64",
    }:
        errors.append("nativePath must publish the lossless five-field envelope")
    compatibility = schema.get("x-compatibility", {})
    if compatibility.get("current") != "1.1" or "additive" not in str(
        compatibility.get("minor", "")
    ):
        errors.append("schema must publish additive-minor compatibility policy")
    if set(schema.get("properties", {}).get("status", {}).get("enum", [])) != STATUSES:
        errors.append("schema status enum differs from the stable emitter status set")
    if schema.get("x-exit-codes") != EXIT_CODES:
        errors.append("schema x-exit-codes differs from the stable 0,2..9 contract")
    rust_constants = {
        name.lower().removeprefix("exit_"): int(value)
        for name, value in re.findall(r"pub const (EXIT_[A-Z_]+): i32 = (\d+);", source)
    }
    for name, expected in EXIT_CODES.items():
        if name == "success":
            continue
        if rust_constants.get(name) != expected:
            errors.append(f"CLI source exit code {name} differs from published {expected}")

    for sample_path in args.sample:
        try:
            raw_sample = sample_path.read_text(encoding="utf-8-sig")
            lines = [line for line in raw_sample.splitlines() if line.strip()]
            if len(lines) != 1:
                raise ValueError(
                    f"stdout must contain exactly one non-empty line, got {len(lines)}"
                )
            sample = json.loads(lines[0])
        except (OSError, ValueError, json.JSONDecodeError) as error:
            errors.append(f"{sample_path}: {error}")
            continue
        if not isinstance(sample, dict):
            errors.append(f"{sample_path}: envelope must be a JSON object")
        else:
            errors.extend(
                f"{sample_path}: schema: {error}"
                for error in validate_against_schema(schema, sample, schema)
            )
            errors.extend(validate_sample(schema, sample, str(sample_path)))

    if errors:
        print("CLI schema check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    samples = ", ".join(str(path) for path in args.sample)
    print(f"CLI schema contract verified: v{version}.{minor}, samples={samples}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
