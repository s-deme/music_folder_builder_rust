#!/usr/bin/env python3
"""Self-test for the repository traceability checker."""

from __future__ import annotations

from pathlib import Path

from check_traceability import check_repository, status_expectations


def main() -> int:
    scripts = Path(__file__).resolve().parent
    repository = scripts.parent
    negative = scripts / "fixtures" / "traceability-negative"

    completed = status_expectations("T01〜T02はすべて完了しています。")
    if completed != {"T01": True, "T02": True}:
        raise AssertionError(f"completed task range was not parsed: {completed}")

    project_findings = check_repository(repository)
    if project_findings:
        rendered = "\n".join(finding.render() for finding in project_findings)
        raise AssertionError(f"real repository must pass traceability validation:\n{rendered}")

    negative_findings = check_repository(negative)
    codes = {finding.code for finding in negative_findings}
    expected = {
        "REQUIREMENT_NOT_TRACED",
        "DESIGN_FILE_MISSING",
        "TEST_EVIDENCE_MISSING",
        "TASK_STATUS_MISMATCH",
        "EVIDENCE_NOT_REACHABLE",
        "EVIDENCE_MARKER_NOT_TEST",
    }
    missing = expected - codes
    if missing:
        rendered = "\n".join(finding.render() for finding in negative_findings)
        raise AssertionError(
            f"negative fixture did not trigger {sorted(missing)}; findings were:\n{rendered}"
        )

    print("traceability self-test passed (positive repository + negative fixture)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
