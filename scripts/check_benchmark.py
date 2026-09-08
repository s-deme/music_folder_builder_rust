#!/usr/bin/env python3
"""Apply the versioned 100k Scan/Plan/dry-run benchmark policy."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from check_cli_schema import validate_against_schema


PHASES = ("cold", "warm", "plan", "apply_dry_run")


def number(value: object) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def median_upper(values: list[int]) -> int:
    return sorted(values)[len(values) // 2]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument(
        "--policy",
        type=Path,
        default=Path(__file__).resolve().with_name("benchmark-policy.json"),
    )
    args = parser.parse_args(argv)

    try:
        envelope = json.loads(args.report.read_text(encoding="utf-8-sig"))
        policy = json.loads(args.policy.read_text(encoding="utf-8"))
        schema_path = Path(__file__).resolve().parents[1] / "docs" / "release" / "cli-envelope.v1.schema.json"
        envelope_schema = json.loads(schema_path.read_text(encoding="utf-8"))
        data = envelope["data"]
        iterations = data["iterations"]
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        print(f"invalid benchmark report: {error}", file=sys.stderr)
        return 1

    errors: list[str] = []
    errors.extend(
        f"published CLI schema: {error}"
        for error in validate_against_schema(envelope_schema, envelope, envelope_schema)
    )
    if envelope.get("schema_version") != 1:
        errors.append("envelope schema_version must be 1")
    if envelope.get("schema_revision") != {"major": 1, "minor": 1}:
        errors.append("envelope schema_revision must be {major:1,minor:1}")
    if envelope.get("command") != "benchmark" or envelope.get("status") != "success":
        errors.append("benchmark JSON envelope must report benchmark/success")
    if envelope.get("result_type") != "benchmark" or envelope.get("result") != data:
        errors.append("benchmark result_type/result alias differs from data")
    if policy.get("schema_version") != 2:
        errors.append("benchmark policy schema_version must be 2")
    if data.get("benchmark_schema_version") != policy.get("benchmark_schema_version"):
        errors.append("benchmark_schema_version differs from policy")

    corpus_items = policy.get("corpus_items")
    minimum_iterations = policy.get("minimum_iterations")
    maximum_iterations = policy.get("maximum_iterations")
    iteration_count = data.get("iteration_count")
    if not isinstance(corpus_items, int) or corpus_items <= 0:
        errors.append("policy corpus_items must be positive")
    if not isinstance(minimum_iterations, int) or minimum_iterations < 3:
        errors.append("policy minimum_iterations must be at least 3")
    if not isinstance(maximum_iterations, int) or not isinstance(minimum_iterations, int) or maximum_iterations < minimum_iterations:
        errors.append("policy maximum_iterations must be >= minimum_iterations")
    if (
        not isinstance(iterations, list)
        or not isinstance(iteration_count, int)
        or not isinstance(minimum_iterations, int)
        or not isinstance(maximum_iterations, int)
        or not minimum_iterations <= iteration_count <= maximum_iterations
        or len(iterations) != iteration_count
    ):
        errors.append("iterations must match iteration_count within the policy range")
        iterations = iterations if isinstance(iterations, list) else []

    max_elapsed = policy.get("max_elapsed_ms", {})
    min_rate = policy.get("min_items_per_second", {})
    observed_elapsed: dict[str, list[int]] = {phase: [] for phase in PHASES}
    observed_peak: list[int] = []
    for index, iteration in enumerate(iterations, start=1):
        if not isinstance(iteration, dict):
            errors.append(f"iterations[{index}] must be an object")
            continue
        if iteration.get("iteration") != index:
            errors.append(f"iterations[{index}].iteration must be {index}")
        for phase_name in PHASES:
            phase = iteration.get(phase_name)
            if not isinstance(phase, dict):
                errors.append(f"iterations[{index}].{phase_name} must be an object")
                continue
            if phase_name in ("cold", "warm"):
                items = phase.get("files")
            elif phase_name == "plan":
                items = phase.get("items")
            else:
                counts = [phase.get(name) for name in ("success", "skipped", "failed")]
                items = sum(counts) if all(isinstance(value, int) for value in counts) else None
                maximum_failed = policy.get("max_apply_failed")
                if not isinstance(maximum_failed, int) or maximum_failed < 0:
                    errors.append("policy max_apply_failed must be non-negative")
                elif not isinstance(phase.get("failed"), int) or phase["failed"] > maximum_failed:
                    errors.append(
                        f"iterations[{index}].apply_dry_run.failed exceeds {maximum_failed}"
                    )
            if items != corpus_items:
                errors.append(
                    f"iterations[{index}].{phase_name} item count must equal {corpus_items}"
                )
            elapsed = phase.get("elapsed_ms")
            ceiling = max_elapsed.get(phase_name)
            if not isinstance(ceiling, int) or ceiling <= 0:
                errors.append(f"policy max_elapsed_ms.{phase_name} must be positive")
            elif not isinstance(elapsed, int) or not 0 <= elapsed <= ceiling:
                errors.append(
                    f"iterations[{index}].{phase_name}.elapsed_ms exceeds {ceiling}"
                )
            else:
                observed_elapsed[phase_name].append(elapsed)
            rate = phase.get("items_per_second")
            floor = min_rate.get(phase_name)
            if not number(floor) or floor <= 0:
                errors.append(f"policy min_items_per_second.{phase_name} must be positive")
            elif not number(rate) or rate < floor:
                errors.append(
                    f"iterations[{index}].{phase_name}.items_per_second must be at least {floor}"
                )
        warm = iteration.get("warm", {})
        if isinstance(warm, dict):
            cache_hits = warm.get("cache_hits")
            tag_reads = warm.get("tag_reads")
            cache_rate = warm.get("cache_hit_rate")
            minimum_cache = policy.get("min_warm_cache_hit_rate")
            if not isinstance(cache_hits, int) or not isinstance(tag_reads, int):
                errors.append(f"iterations[{index}].warm cache counters must be integers")
            elif cache_hits + tag_reads != corpus_items:
                errors.append(f"iterations[{index}].warm cache counters do not sum to corpus")
            if not number(minimum_cache) or not 0 <= minimum_cache <= 1:
                errors.append("policy min_warm_cache_hit_rate must be within 0..1")
            elif not number(cache_rate) or cache_rate < minimum_cache:
                errors.append(
                    f"iterations[{index}].warm.cache_hit_rate must be at least {minimum_cache:.2f}"
                )
        peak = iteration.get("peak_rss_bytes")
        if not isinstance(peak, int) or peak <= 0:
            errors.append(f"iterations[{index}].peak_rss_bytes must be positive")
        else:
            observed_peak.append(peak)

    for phase_name in PHASES:
        summary = data.get(phase_name)
        if not isinstance(summary, dict):
            errors.append(f"{phase_name} summary must be an object")
            continue
        if summary.get("statistic") != "median":
            errors.append(f"{phase_name}.statistic must be median")
        values = observed_elapsed[phase_name]
        if len(values) == len(iterations) and values:
            expected = median_upper(values)
            if summary.get("elapsed_ms") != expected:
                errors.append(f"{phase_name}.elapsed_ms must equal iteration median {expected}")
        floor = min_rate.get(phase_name)
        if not number(summary.get("items_per_second")) or (
            number(floor) and summary["items_per_second"] < floor
        ):
            errors.append(f"{phase_name} summary rate violates policy")
    for phase_name in ("cold", "warm"):
        if isinstance(data.get(phase_name), dict) and data[phase_name].get("files") != corpus_items:
            errors.append(f"{phase_name}.files must equal corpus_items")
    if isinstance(data.get("warm"), dict):
        minimum_cache = policy.get("min_warm_cache_hit_rate")
        if not number(data["warm"].get("cache_hit_rate")) or (
            number(minimum_cache) and data["warm"]["cache_hit_rate"] < minimum_cache
        ):
            errors.append("warm summary cache_hit_rate violates policy")

    peak = data.get("peak_rss_bytes")
    baseline = data.get("baseline_rss_bytes")
    growth = data.get("rss_growth_bytes")
    max_peak = policy.get("max_peak_rss_bytes")
    max_growth = policy.get("max_rss_growth_bytes")
    if observed_peak and peak != max(observed_peak):
        errors.append("peak_rss_bytes must equal the largest iteration peak")
    if data.get("rss_bytes") != peak:
        errors.append("rss_bytes compatibility alias must equal peak_rss_bytes")
    if not isinstance(peak, int) or not isinstance(max_peak, int) or not 0 < peak <= max_peak:
        errors.append(f"peak_rss_bytes must be within 1..{max_peak}")
    if not isinstance(baseline, int) or baseline < 0:
        errors.append("baseline_rss_bytes must be non-negative")
    elif not isinstance(peak, int) or growth != max(0, peak - baseline):
        errors.append("rss_growth_bytes is inconsistent with peak and baseline")
    if not isinstance(growth, int) or not isinstance(max_growth, int) or not 0 <= growth <= max_growth:
        errors.append(f"rss_growth_bytes must be within 0..{max_growth}")
    sampling = data.get("rss_sampling_interval_ms")
    max_sampling = policy.get("max_rss_sampling_interval_ms")
    if not isinstance(sampling, int) or not isinstance(max_sampling, int) or not 0 < sampling <= max_sampling:
        errors.append(f"rss_sampling_interval_ms must be within 1..{max_sampling}")
    if data.get("workspace_ephemeral") is not True:
        errors.append("workspace_ephemeral must be true")

    if errors:
        print("benchmark regression gate failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    medians = " ".join(f"{phase}={data[phase]['elapsed_ms']}ms" for phase in PHASES)
    print(
        f"benchmark gate passed: items={corpus_items} iterations={iteration_count} "
        f"{medians} peak_rss={peak} policy={args.policy}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
