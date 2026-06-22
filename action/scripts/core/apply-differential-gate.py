#!/usr/bin/env python3
"""Downgrade audit/test failures when PR metrics do not exceed base metrics."""

from __future__ import annotations

import json
import os
import sys
from typing import Any


def read_json(path: str) -> dict[str, Any] | None:
    try:
        with open(path, "r", encoding="utf-8") as handle:
            payload = json.load(handle)
        return payload if isinstance(payload, dict) else None
    except (OSError, json.JSONDecodeError):
        return None


def unwrap(payload: dict[str, Any] | None) -> dict[str, Any]:
    if not payload:
        return {}
    if "success" in payload and isinstance(payload.get("data"), dict):
        return payload["data"]
    return payload


def audit_count(payload: dict[str, Any] | None) -> int | None:
    data = unwrap(payload)
    summary = data.get("summary", {}) if isinstance(data, dict) else {}
    if isinstance(summary, dict) and isinstance(summary.get("outliers_found"), int):
        return int(summary["outliers_found"])

    findings = data.get("findings", []) if isinstance(data, dict) else []
    if isinstance(findings, list) and findings:
        return len(findings)

    conventions = data.get("conventions", []) if isinstance(data, dict) else []
    if isinstance(conventions, list):
        total = 0
        saw_conventions = False
        for convention in conventions:
            if not isinstance(convention, dict):
                continue
            saw_conventions = True
            outliers = convention.get("outliers", [])
            if isinstance(outliers, list):
                total += len(outliers)
        if saw_conventions:
            return total

    baseline = data.get("baseline_comparison", {}) if isinstance(data, dict) else {}
    new_items = baseline.get("new_items", []) if isinstance(baseline, dict) else []
    if isinstance(baseline, dict) and "new_items" in baseline and isinstance(new_items, list):
        return len(new_items)

    return None


def test_count(payload: dict[str, Any] | None) -> int | None:
    data = unwrap(payload)
    counts = data.get("test_counts", {}) if isinstance(data, dict) else {}
    if isinstance(counts, dict):
        failed = int(counts.get("failed", 0) or 0)
        errors = int(counts.get("errors", 0) or 0)
        if failed or errors or any(key in counts for key in ("failed", "errors")):
            return failed + errors

    failed_tests = data.get("failed_tests", []) if isinstance(data, dict) else []
    if isinstance(data, dict) and "failed_tests" in data and isinstance(failed_tests, list):
        return len(failed_tests)

    summary = data.get("summary", {}) if isinstance(data, dict) else {}
    if isinstance(summary, dict) and isinstance(summary.get("failures"), int):
        return int(summary["failures"])

    return None


def lint_count(payload: dict[str, Any] | None) -> int | None:
    data = unwrap(payload)
    if not isinstance(data, dict):
        return None

    for key in ("lint_findings", "findings", "violations", "top_violations"):
        items = data.get(key)
        if isinstance(items, list):
            return len(items)

    summary = data.get("summary")
    if isinstance(summary, dict):
        for key in ("findings", "failures", "errors"):
            value = summary.get(key)
            if isinstance(value, int):
                return int(value)

    return None


def changed_scope_introduced_count(command: str, payload: dict[str, Any] | None) -> int | None:
    data = unwrap(payload)
    changed_since = data.get("changed_since", {}) if isinstance(data, dict) else {}
    if not isinstance(changed_since, dict):
        return None

    key = "introduced_findings" if command == "audit" else "introduced_failures"
    value = changed_since.get(key)
    return int(value) if isinstance(value, int) else None


def output_stem(command: str) -> str:
    return "".join(ch if ch.isalnum() or ch in "._-" else "-" for ch in command).strip("-") or "homeboy-output"


def metric_for(command: str, directory: str) -> int | None:
    payload = read_json(os.path.join(directory, f"{output_stem(command)}.json"))
    scoped_count = changed_scope_introduced_count(command, payload)
    if scoped_count is not None:
        return scoped_count
    if command == "audit":
        return audit_count(payload)
    if command == "lint":
        return lint_count(payload)
    if command == "test":
        return test_count(payload)
    return None


def base_command_status(command: str, base_dir: str) -> dict[str, Any]:
    status = read_json(os.path.join(base_dir, "baseline-status.json"))
    if not isinstance(status, dict):
        return {}
    value = status.get(command)
    return value if isinstance(value, dict) else {}


def command_label(command: str, metadata: dict[str, Any]) -> str:
    full = metadata.get("command")
    if isinstance(full, str) and full:
        return full
    return f"homeboy {command}"


def command_failed(metadata: dict[str, Any]) -> bool:
    if metadata.get("status") == "fail":
        return True
    try:
        return int(metadata.get("exit_code") or 0) != 0
    except (TypeError, ValueError):
        return False


def main() -> int:
    if len(sys.argv) != 4:
        print("usage: apply-differential-gate.py RESULTS_JSON CURRENT_DIR BASE_DIR", file=sys.stderr)
        return 2

    results = json.loads(sys.argv[1] or "{}")
    current_dir = sys.argv[2]
    base_dir = sys.argv[3]

    if not isinstance(results, dict):
        print(sys.argv[1])
        return 0

    adjusted = dict(results)
    for command in ("audit", "lint", "test"):
        if adjusted.get(command) != "fail":
            continue

        current = metric_for(command, current_dir)
        base = metric_for(command, base_dir)
        base_metadata = base_command_status(command, base_dir)
        base_failed = command_failed(base_metadata)
        base_structured = base_metadata.get("structured_output") is True

        if base_failed and (base is None or not base_structured):
            adjusted[command] = "baseline_red"
            print(
                f"::warning::Differential gate marked {command} baseline_red: baseline command `{command_label(command, base_metadata)}` exited {base_metadata.get('exit_code', 'unknown')} before comparable counts were available",
                file=sys.stderr,
            )
            continue

        if current is None or base is None:
            adjusted[command] = "inconclusive"
            print(
                f"::warning::Differential gate marked {command} inconclusive: current={current if current is not None else 'unavailable'} base={base if base is not None else 'unavailable'}",
                file=sys.stderr,
            )
            continue

        if current <= base:
            adjusted[command] = "pass"
            print(
                f"::notice::Differential gate accepted {command}: current={current} base={base}",
                file=sys.stderr,
            )
        else:
            print(
                f"::error::Differential gate rejected {command}: current={current} base={base}",
                file=sys.stderr,
            )

    print(json.dumps(adjusted, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
