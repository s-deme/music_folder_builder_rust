#!/usr/bin/env python3
"""Validate requirement/design/task/test traceability and task status consistency."""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path


REQ_PATTERN = re.compile(
    r"REQ-([A-Z]+)-(\d{3})(?:\s*[〜~]\s*(?:(?:REQ-)?([A-Z]+)-)?(\d{3}))?"
)
TASK_PATTERN = re.compile(r"T(\d{2,})(?:\s*[〜~]\s*(?:T)?(\d{2,}))?")
TEST_EVIDENCE_PATTERN = re.compile(
    r"(?:test|検証|benchmark|inspection|smoke|golden|matrix|snapshot|fixture)", re.I
)
PLACEHOLDER_PATTERN = re.compile(r"^(?:-|なし|n/?a|todo|tbd|未定)$", re.I)


@dataclass(frozen=True, order=True)
class Finding:
    code: str
    message: str

    def render(self) -> str:
        return f"[{self.code}] {self.message}"


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return ""


def expand_requirements(value: str) -> set[str]:
    expanded: set[str] = set()
    for match in REQ_PATTERN.finditer(value):
        family, start_text, end_family, end_text = match.groups()
        start = int(start_text)
        if end_text is None:
            expanded.add(f"REQ-{family}-{start:03d}")
            continue
        effective_end_family = end_family or family
        if effective_end_family != family:
            continue
        end = int(end_text)
        if end < start:
            continue
        expanded.update(f"REQ-{family}-{number:03d}" for number in range(start, end + 1))
    return expanded


def expand_tasks(value: str) -> set[str]:
    expanded: set[str] = set()
    for match in TASK_PATTERN.finditer(value):
        start = int(match.group(1))
        end = int(match.group(2) or match.group(1))
        if end < start:
            continue
        expanded.update(f"T{number:02d}" for number in range(start, end + 1))
    return expanded


def markdown_rows(text: str) -> list[list[str]]:
    rows: list[list[str]] = []
    for line in text.splitlines():
        if not line.lstrip().startswith("|"):
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if len(cells) < 4 or cells[0] in {"要件", "---"}:
            continue
        if all(re.fullmatch(r":?-{3,}:?", cell) for cell in cells):
            continue
        if "REQ-" in cells[0]:
            rows.append(cells)
    return rows


def parse_task_plan(text: str) -> tuple[dict[str, bool], list[Finding]]:
    states: dict[str, bool] = {}
    findings: list[Finding] = []
    patterns = [
        re.compile(r"^-\s*\[([xX ])\]\s*(T\d{2,})(?=\D|$)"),
        re.compile(r"^\|\s*\[([xX ])\]\s*\|\s*(T\d{2,})(?=\D|$)"),
    ]
    for line_number, line in enumerate(text.splitlines(), start=1):
        match = next((pattern.search(line) for pattern in patterns if pattern.search(line)), None)
        if not match:
            continue
        checked = match.group(1).lower() == "x"
        task = match.group(2)
        if task in states and states[task] != checked:
            findings.append(
                Finding(
                    "TASK_DUPLICATE_STATE",
                    f"{task} has conflicting checkbox states near line {line_number}",
                )
            )
        states[task] = checked
    return states, findings


def status_expectations(text: str) -> dict[str, bool]:
    """Return task -> expected completed state for explicit status statements."""
    expectations: dict[str, bool] = {}
    for line in text.splitlines():
        row = re.match(r"^\|\s*(T\d{2,})\s*\|\s*([^|]+)", line)
        if row:
            label = row.group(2).strip()
            if label.startswith("完了"):
                expectations[row.group(1)] = True
            elif "部分実装" in label or "未実装" in label or "未完了" in label:
                expectations[row.group(1)] = False
        if "未完了" in line or "未実装" in line:
            for task in expand_tasks(line):
                expectations[task] = False
        elif re.search(r"すべて完了(?:です|しています|。|$)", line):
            for task in expand_tasks(line):
                expectations[task] = True
    return expectations


def resolve_design_reference(root: Path, reference: str) -> bool:
    design_root = (root / "storage" / "design").resolve()
    if "/" in reference or "\\" in reference:
        candidate = (design_root / reference).resolve()
        try:
            candidate.relative_to(design_root)
        except ValueError:
            return False
        return candidate.suffix.lower() == ".md" and candidate.is_file()
    return any(path.name == reference for path in design_root.rglob("*.md"))


def evidence_manifest(root: Path) -> tuple[dict[frozenset[str], dict[str, object]], list[Finding]]:
    path = root / "scripts" / "traceability-evidence.json"
    findings: list[Finding] = []
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}, [Finding("EVIDENCE_MANIFEST_MISSING", f"missing {path}")]
    except json.JSONDecodeError as error:
        return {}, [Finding("EVIDENCE_MANIFEST_INVALID", f"invalid {path}: {error}")]
    if (
        document.get("schema_version") != 1
        or not isinstance(document.get("claims"), list)
        or not isinstance(document.get("execution_profiles"), list)
    ):
        return {}, [Finding("EVIDENCE_MANIFEST_INVALID", f"unsupported schema in {path}")]

    manifest: dict[frozenset[str], dict[str, object]] = {}
    repository = root.resolve()
    profiles: list[dict[str, str]] = []
    for profile_index, raw_profile in enumerate(document["execution_profiles"], start=1):
        if not isinstance(raw_profile, dict):
            findings.append(
                Finding("EVIDENCE_PROFILE_INVALID", f"execution profile {profile_index} is not an object")
            )
            continue
        profile = {name: str(raw_profile.get(name, "")) for name in (
            "name", "path", "path_prefix", "path_suffix", "kind", "gate_path", "gate_contains"
        )}
        if (
            not profile["name"]
            or not profile["kind"]
            or profile["kind"] not in {"rust-test", "ui-test", "script", "workflow"}
            or not profile["gate_path"]
            or not profile["gate_contains"]
            or not (profile["path"] or profile["path_prefix"])
        ):
            findings.append(
                Finding("EVIDENCE_PROFILE_INVALID", f"execution profile {profile_index} is incomplete")
            )
            continue
        gate = (repository / profile["gate_path"]).resolve()
        try:
            gate.relative_to(repository)
        except ValueError:
            findings.append(
                Finding("EVIDENCE_GATE_PATH_ESCAPE", f"profile {profile['name']} gate escapes repository")
            )
            continue
        if not gate.is_file():
            findings.append(
                Finding("EVIDENCE_GATE_MISSING", f"profile {profile['name']} gate is missing: {profile['gate_path']}")
            )
            continue
        try:
            gate_text = gate.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            findings.append(
                Finding("EVIDENCE_GATE_NOT_TEXT", f"profile {profile['name']} gate is not UTF-8")
            )
            continue
        if profile["gate_contains"] not in gate_text:
            findings.append(
                Finding(
                    "EVIDENCE_GATE_MARKER_MISSING",
                    f"profile {profile['name']} gate does not invoke {profile['gate_contains']!r}",
                )
            )
            continue
        profiles.append(profile)

    def matching_profiles(relative: str) -> list[dict[str, str]]:
        matches = []
        for profile in profiles:
            exact = profile["path"] and relative == profile["path"]
            prefix = profile["path_prefix"] and relative.startswith(profile["path_prefix"])
            suffix = not profile["path_suffix"] or relative.endswith(profile["path_suffix"])
            if suffix and (exact or prefix):
                matches.append(profile)
        return matches

    for index, claim in enumerate(document["claims"], start=1):
        if not isinstance(claim, dict):
            findings.append(Finding("EVIDENCE_MANIFEST_INVALID", f"claim {index} is not an object"))
            continue
        requirements = frozenset(expand_requirements(str(claim.get("requirements", ""))))
        if not requirements:
            findings.append(Finding("EVIDENCE_MANIFEST_INVALID", f"claim {index} has no requirements"))
            continue
        if requirements in manifest:
            findings.append(
                Finding("EVIDENCE_MANIFEST_DUPLICATE", f"claim {index} duplicates a requirement set")
            )
        manifest[requirements] = claim
        artifacts = claim.get("artifacts")
        if not isinstance(artifacts, list) or not artifacts:
            findings.append(Finding("EVIDENCE_ARTIFACT_MISSING", f"claim {index} has no artifacts"))
            continue
        for artifact_index, artifact in enumerate(artifacts, start=1):
            if not isinstance(artifact, dict):
                findings.append(
                    Finding(
                        "EVIDENCE_ARTIFACT_INVALID",
                        f"claim {index} artifact {artifact_index} is not an object",
                    )
                )
                continue
            relative = str(artifact.get("path", ""))
            marker = str(artifact.get("contains", ""))
            candidate = (repository / relative).resolve()
            try:
                candidate.relative_to(repository)
            except ValueError:
                findings.append(
                    Finding("EVIDENCE_PATH_ESCAPE", f"claim {index} artifact escapes repository: {relative}")
                )
                continue
            if not candidate.is_file():
                findings.append(
                    Finding("EVIDENCE_FILE_MISSING", f"claim {index} references missing {relative}")
                )
                continue
            if not marker:
                findings.append(
                    Finding("EVIDENCE_MARKER_MISSING", f"claim {index} artifact {relative} has no marker")
                )
                continue
            try:
                artifact_text = candidate.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                findings.append(
                    Finding("EVIDENCE_FILE_NOT_TEXT", f"claim {index} artifact is not UTF-8 text: {relative}")
                )
                continue
            if marker not in artifact_text:
                findings.append(
                    Finding(
                        "EVIDENCE_MARKER_NOT_FOUND",
                        f"claim {index} marker {marker!r} is absent from {relative}",
                    )
                )
            normalized = candidate.relative_to(repository).as_posix()
            artifact_profiles = matching_profiles(normalized)
            if len(artifact_profiles) != 1:
                findings.append(
                    Finding(
                        "EVIDENCE_NOT_REACHABLE",
                        f"claim {index} artifact must match exactly one execution profile: {relative}",
                    )
                )
                continue
            kind = artifact_profiles[0]["kind"]
            if kind == "rust-test":
                test_pattern = re.compile(
                    r"#\[(?:[A-Za-z_][A-Za-z0-9_]*::)?test\]"
                    r"(?:\s*#\[[^\]]+\])*\s*fn\s+" + re.escape(marker) + r"\b"
                )
                if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", marker) or not test_pattern.search(artifact_text):
                    findings.append(
                        Finding(
                            "EVIDENCE_MARKER_NOT_TEST",
                            f"claim {index} marker is not the named Rust test in {relative}: {marker!r}",
                        )
                    )
            elif kind == "ui-test":
                declaration = re.compile(
                    r"\btest\s*\(\s*([\"'])[^\"']*" + re.escape(marker) + r"[^\"']*\1"
                )
                if not declaration.search(artifact_text):
                    findings.append(
                        Finding(
                            "EVIDENCE_MARKER_NOT_TEST",
                            f"claim {index} marker is not a declared UI test in {relative}: {marker!r}",
                        )
                    )
    return manifest, findings


def check_repository(root: Path, *, require_complete: bool = False) -> list[Finding]:
    findings: list[Finding] = []
    spec_root = root / "storage" / "specs"
    traceability_path = root / "storage" / "design" / "traceability.ja.md"
    task_path = root / "storage" / "tasks" / "implementation-plan.ja.md"
    status_path = root / "IMPLEMENTATION_STATUS.ja.md"

    spec_requirements: set[str] = set()
    for spec in sorted(spec_root.glob("*.md")):
        spec_requirements.update(
            match.group(1)
            for match in re.finditer(r"^###\s+(REQ-[A-Z]+-\d{3})(?=:)", read_text(spec), re.M)
        )
    if not spec_requirements:
        findings.append(Finding("SPEC_EMPTY", f"no EARS requirement IDs found under {spec_root}"))

    task_states, task_findings = parse_task_plan(read_text(task_path))
    findings.extend(task_findings)
    if not task_states:
        findings.append(Finding("TASK_PLAN_EMPTY", f"no task checkbox IDs found in {task_path}"))
    evidence, evidence_findings = evidence_manifest(root)
    findings.extend(evidence_findings)
    used_evidence: set[frozenset[str]] = set()

    coverage: Counter[str] = Counter()
    traceability_text = read_text(traceability_path)
    rows = markdown_rows(traceability_text)
    if not rows:
        findings.append(
            Finding("TRACEABILITY_EMPTY", f"no requirement rows found in {traceability_path}")
        )

    for row_number, cells in enumerate(rows, start=1):
        requirement_cell, design_cell, task_cell, test_cell = cells[:4]
        requirements = expand_requirements(requirement_cell)
        requirement_key = frozenset(requirements)
        tasks = expand_tasks(task_cell)
        for requirement in requirements:
            coverage[requirement] += 1
            if requirement not in spec_requirements:
                findings.append(
                    Finding(
                        "TRACEABILITY_UNKNOWN_REQUIREMENT",
                        f"row {row_number} references {requirement}, which is absent from specs",
                    )
                )

        if not design_cell or PLACEHOLDER_PATTERN.fullmatch(design_cell):
            findings.append(
                Finding("DESIGN_EVIDENCE_MISSING", f"row {row_number} has no design evidence")
            )
        design_files = re.findall(r"`([^`]+\.md)`", design_cell)
        adr_numbers = re.findall(r"\bADR-(\d{3})\b", design_cell)
        if not design_files and not adr_numbers:
            findings.append(
                Finding(
                    "DESIGN_REFERENCE_MISSING",
                    f"row {row_number} has no resolvable Markdown/ADR reference",
                )
            )
        for reference in design_files:
            if not resolve_design_reference(root, reference):
                findings.append(
                    Finding(
                        "DESIGN_FILE_MISSING",
                        f"row {row_number} references missing design file {reference}",
                    )
                )
        for number in adr_numbers:
            if not any((root / "storage" / "design" / "adr").glob(f"{number}-*.md")):
                findings.append(
                    Finding(
                        "ADR_FILE_MISSING", f"row {row_number} references missing ADR-{number}"
                    )
                )

        if not tasks:
            findings.append(Finding("TASK_EVIDENCE_MISSING", f"row {row_number} has no task IDs"))
        for task in tasks:
            if task not in task_states:
                findings.append(
                    Finding(
                        "TRACEABILITY_UNKNOWN_TASK",
                        f"row {row_number} references {task}, which is absent from the task plan",
                    )
                )

        if (
            not test_cell
            or PLACEHOLDER_PATTERN.fullmatch(test_cell)
            or not TEST_EVIDENCE_PATTERN.search(test_cell)
        ):
            findings.append(
                Finding(
                    "TEST_EVIDENCE_MISSING",
                    f"row {row_number} lacks concrete automated test/inspection evidence",
                )
            )
        machine_evidence = evidence.get(requirement_key)
        if machine_evidence is None:
            findings.append(
                Finding(
                    "MACHINE_EVIDENCE_MISSING",
                    f"row {row_number} has no scripts/traceability-evidence.json entry",
                )
            )
        else:
            used_evidence.add(requirement_key)
            if machine_evidence.get("claim") != test_cell:
                findings.append(
                    Finding(
                        "EVIDENCE_CLAIM_MISMATCH",
                        f"row {row_number} prose differs from its machine evidence claim",
                    )
                )

    for requirement in sorted(spec_requirements):
        count = coverage[requirement]
        if count == 0:
            findings.append(
                Finding("REQUIREMENT_NOT_TRACED", f"{requirement} has no traceability row")
            )
        elif count > 1:
            findings.append(
                Finding(
                    "REQUIREMENT_DUPLICATE_TRACE",
                    f"{requirement} appears in {count} traceability rows",
                )
            )

    for unused in evidence.keys() - used_evidence:
        findings.append(
            Finding(
                "EVIDENCE_ENTRY_UNUSED",
                "machine evidence entry is not referenced by a traceability row: "
                + ",".join(sorted(unused)),
            )
        )

    expectations = status_expectations(read_text(status_path))
    for task in sorted(task_states.keys() - expectations.keys()):
        findings.append(
            Finding("STATUS_TASK_OMITTED", f"status document does not classify task {task}")
        )
    for task, expected_completed in sorted(expectations.items()):
        actual = task_states.get(task)
        if actual is None:
            findings.append(
                Finding("STATUS_UNKNOWN_TASK", f"status document references missing task {task}")
            )
        elif actual != expected_completed:
            expected_label = "completed" if expected_completed else "incomplete"
            actual_label = "completed" if actual else "incomplete"
            findings.append(
                Finding(
                    "TASK_STATUS_MISMATCH",
                    f"{task} is {actual_label} in task plan but {expected_label} in status document",
                )
            )

    if require_complete:
        for task, completed in sorted(task_states.items()):
            if not completed:
                findings.append(
                    Finding(
                        "TASK_INCOMPLETE",
                        f"{task} is incomplete; release readiness requires every repository task",
                    )
                )

    return sorted(set(findings))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="repository root (defaults to the parent of scripts/)",
    )
    parser.add_argument(
        "--require-complete",
        action="store_true",
        help="also fail when any implementation-plan task remains unchecked",
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()
    findings = check_repository(root, require_complete=args.require_complete)
    if findings:
        print(f"traceability check failed with {len(findings)} finding(s):", file=sys.stderr)
        for finding in findings:
            print(f"  {finding.render()}", file=sys.stderr)
        return 1
    print(f"traceability check passed: {root}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
