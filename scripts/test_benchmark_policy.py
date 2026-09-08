#!/usr/bin/env python3
"""Self-test that functional benchmark regressions cannot pass on speed alone."""

from __future__ import annotations

import contextlib
import copy
import io
import json
import tempfile
from pathlib import Path

import check_benchmark


def phase(elapsed_ms: int) -> dict[str, object]:
    return {"elapsed_ms": elapsed_ms, "items_per_second": 1000.0}


def report() -> dict[str, object]:
    iterations = []
    for number in range(1, 4):
        cold = {**phase(100 + number), "files": 100_000, "cache_hits": 0, "tag_reads": 100_000}
        warm = {
            **phase(50 + number),
            "files": 100_000,
            "cache_hits": 100_000,
            "tag_reads": 0,
            "cache_hit_rate": 1.0,
        }
        plan = {**phase(70 + number), "items": 100_000, "conflicts": 0, "risks": 0}
        apply = {**phase(60 + number), "success": 100_000, "skipped": 0, "failed": 0}
        iterations.append(
            {
                "iteration": number,
                "cold": cold,
                "warm": warm,
                "plan": plan,
                "apply_dry_run": apply,
                "peak_rss_bytes": 100_000_000 + number,
            }
        )
    data = {
        "benchmark_schema_version": 2,
        "iterations": iterations,
        "iteration_count": 3,
        "cold": {**phase(102), "files": 100_000, "statistic": "median"},
        "warm": {
            **phase(52),
            "files": 100_000,
            "cache_hits": 100_000,
            "cache_hit_rate": 1.0,
            "statistic": "median",
        },
        "plan": {**phase(72), "statistic": "median"},
        "apply_dry_run": {**phase(62), "statistic": "median"},
        "rss_bytes": 100_000_003,
        "peak_rss_bytes": 100_000_003,
        "baseline_rss_bytes": 50_000_000,
        "rss_growth_bytes": 50_000_003,
        "rss_sampling_interval_ms": 5,
        "workspace_ephemeral": True,
    }
    return {
        "schema_version": 1,
        "schema_revision": {"major": 1, "minor": 1},
        "command": "benchmark",
        "status": "success",
        "result_type": "benchmark",
        "correlation": {"logical_run_id": None, "attempt_id": None, "subject_id": None},
        "counts": {},
        "diagnostics": [],
        "result": data,
        "data": data,
        "error": None,
    }


def run_case(path: Path, document: dict[str, object]) -> int:
    path.write_text(json.dumps(document), encoding="utf-8")
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
        return check_benchmark.main([str(path)])


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="music-folder-benchmark-policy-") as temporary:
        path = Path(temporary) / "report.json"
        valid = report()
        assert run_case(path, valid) == 0

        failed = copy.deepcopy(valid)
        failed["data"]["iterations"][0]["apply_dry_run"].update(  # type: ignore[index]
            {"success": 99_999, "failed": 1}
        )
        failed["result"] = failed["data"]
        assert run_case(path, failed) == 1

        wrong_median = copy.deepcopy(valid)
        wrong_median["data"]["plan"]["elapsed_ms"] = 1  # type: ignore[index]
        wrong_median["result"] = wrong_median["data"]
        assert run_case(path, wrong_median) == 1
    print("benchmark policy self-test passed (Apply failure + median negatives)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
