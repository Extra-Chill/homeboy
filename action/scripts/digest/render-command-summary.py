#!/usr/bin/env python3
"""Render command summaries from homeboy structured JSON output.

Reads the raw CliResponse<T> envelope written by `homeboy --output`.
Produces compact (one-line) or markdown summaries for PR comments and
issue filing.
"""
from __future__ import annotations

import json
import sys
from typing import Any


def load_json(path: str) -> dict[str, Any]:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    return data if isinstance(data, dict) else {}


def unwrap_envelope(raw: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    """Unwrap a CliResponse envelope into (data, error).

    Handles both the raw CLI envelope ({"success": bool, "data": {...}})
    and pre-normalized dicts (no envelope).
    """
    if "success" in raw and ("data" in raw or "error" in raw):
        return raw.get("data", {}) or {}, raw.get("error", {}) or {}
    # Already unwrapped or flat dict
    return raw, {}


def append_details(lines: list[str], summary: str, body_lines: list[str]) -> None:
    content = [str(line) for line in body_lines if str(line).strip()]
    if not content:
        return
    lines.append("")
    lines.append(f"<details><summary>{summary}</summary>")
    lines.append("")
    lines.append("```text")
    lines.extend(content)
    lines.append("```")
    lines.append("")
    lines.append("</details>")


def format_new_audit_item(item: dict[str, Any]) -> str:
    context = str(item.get("context_label") or item.get("file") or "unknown")
    message = str(item.get("description") or item.get("message") or "(new finding)")
    fingerprint = str(item.get("fingerprint") or "")
    entry = f"**{context}**"
    if message:
        entry += f" — {message}"
    if fingerprint:
        entry += f" (`{fingerprint}`)"
    return entry


# ── Compact summaries (one-line for issue filing) ─────────────────────

def compact_summary(command: str, raw: dict[str, Any]) -> str:
    data, error = unwrap_envelope(raw)

    # Surface errors from any command
    if error and error.get("message"):
        code = error.get("code", "")
        msg = str(error["message"])
        return f"error: {code} — {msg}" if code else f"error: {msg}"

    if command == "lint":
        return compact_lint(data)
    if command == "test":
        return compact_test(data)
    if command == "audit":
        return compact_audit(data)
    if command == "refactor":
        return compact_refactor(data)
    if command == "bench":
        return compact_bench(data)
    return "structured command details available"


def compact_lint(data: dict[str, Any]) -> str:
    for value in [
        data.get("build_failed"),
        data.get("summary"),
        data.get("phpcs_summary"),
        data.get("phpstan_summary"),
    ]:
        if value:
            return str(value)
    top = data.get("top_violations", [])
    if top:
        return f"{len(top)} violation(s)"
    return "structured lint details available"


def compact_test(data: dict[str, Any]) -> str:
    test_counts = data.get("test_counts", {}) or {}
    failed_tests = data.get("failed_tests", []) or []
    failed_count = int(test_counts.get("failed", 0) or 0) + int(test_counts.get("errors", 0) or 0)
    if not failed_count:
        failed_count = len(failed_tests)

    if failed_tests:
        first = failed_tests[0] if isinstance(failed_tests[0], dict) else {"name": str(failed_tests[0])}
        name = str(first.get("name", "unknown")).strip()
        detail = str(first.get("detail", first.get("message", ""))).strip()
        suffix = f" — {name}" if name else ""
        if detail:
            suffix += f": {detail}"
        return f"{failed_count} failed test(s){suffix}"
    return f"{failed_count} failed test(s)"


def compact_audit(data: dict[str, Any]) -> str:
    baseline = data.get("baseline_comparison", {}) or {}
    summary = data.get("summary", {}) or {}
    new_items = baseline.get("new_items", []) if isinstance(baseline, dict) else []
    if isinstance(new_items, list) and new_items:
        return f"{len(new_items)} new finding(s) since baseline"
    outliers = summary.get("outliers_found") if isinstance(summary, dict) else None
    if isinstance(outliers, int):
        return f"{outliers} outlier(s) in current run"
    alignment = summary.get("alignment_score") if isinstance(summary, dict) else None
    if isinstance(alignment, (int, float)):
        return f"alignment score {alignment:.3f}"
    return "structured audit details available"


def compact_refactor(data: dict[str, Any]) -> str:
    files_modified = int(data.get("files_modified", 0) or 0)
    stages = data.get("stages", [])
    totals = data.get("plan_totals", {})
    total_fixes = int(totals.get("total_fixes_proposed", 0) or 0) if isinstance(totals, dict) else 0

    stage_parts: list[str] = []
    for stage in (stages if isinstance(stages, list) else []):
        if not isinstance(stage, dict):
            continue
        name = str(stage.get("stage", "unknown"))
        proposed = int(stage.get("fixes_proposed", 0) or 0)
        if proposed > 0:
            stage_parts.append(f"{name}: {proposed}")

    if stage_parts:
        breakdown = ", ".join(stage_parts)
        return f"{total_fixes} fix(es) across {files_modified} file(s) — {breakdown}"
    if files_modified > 0:
        return f"{total_fixes} fix(es) across {files_modified} file(s)"

    warnings = data.get("warnings", [])
    validation_warnings = [str(w) for w in warnings if isinstance(w, str) and "validation" in w.lower()]
    if validation_warnings:
        return validation_warnings[0]
    return "no automated fixes found"


def compact_bench(data: dict[str, Any]) -> str:
    regressions = count_matching_keys(data, {"regression", "regressions"})
    scenarios = collect_scenario_names(data)
    primary = collect_bench_metric_rows(data)
    if primary:
        return "; ".join(primary[:2])
    if regressions:
        return f"{regressions} regression(s) reported"
    if scenarios:
        return f"{len(scenarios)} benchmark scenario(s) reported"
    return "structured benchmark details available"


# ── Markdown summaries (multi-line for PR comments) ───────────────────

def markdown_summary(command: str, raw: dict[str, Any]) -> str:
    data, error = unwrap_envelope(raw)
    lines: list[str] = []

    # Surface errors from any command
    if error and error.get("message"):
        lines.append(f"- Error: **{error.get('code', 'unknown')}** — {error['message']}")
        hints = error.get("hints", [])
        for hint in (hints if isinstance(hints, list) else [])[:3]:
            lines.append(f"  - Hint: {hint}")
        return "\n".join(lines)

    if command == "lint":
        markdown_lint(data, lines)
    elif command == "test":
        markdown_test(data, lines)
    elif command == "audit":
        markdown_audit(data, lines)
    elif command == "refactor":
        markdown_refactor(data, lines)
    elif command == "bench":
        markdown_bench(data, lines)

    return "\n".join(lines)


def walk_dicts(value: Any):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from walk_dicts(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk_dicts(child)


def count_matching_keys(value: Any, needles: set[str]) -> int:
    count = 0
    for item in walk_dicts(value):
        for key, child in item.items():
            if str(key).lower() in needles:
                if isinstance(child, list):
                    count += len(child)
                elif isinstance(child, dict):
                    count += len(child) or 1
                elif child:
                    count += 1
    return count


def collect_scenario_names(data: dict[str, Any]) -> list[str]:
    names: list[str] = []
    for item in walk_dicts(data):
        for key in ("scenario", "scenario_id", "name", "id"):
            raw = item.get(key)
            if isinstance(raw, str) and raw and any(token in item for token in ("p50", "p95", "elapsed_ms", "metrics", "timings_ns")):
                if raw not in names:
                    names.append(raw)
                break
    return names


def collect_bench_metric_rows(data: dict[str, Any]) -> list[str]:
    rows: list[str] = []
    seen: set[str] = set()
    for item in walk_dicts(data):
        label = str(item.get("scenario") or item.get("scenario_id") or item.get("name") or item.get("id") or "benchmark")
        p50 = metric_value(item, "p50")
        p95 = metric_value(item, "p95")
        delta = metric_value(item, "delta_percent") or metric_value(item, "change_percent") or metric_value(item, "regression_percent")
        elapsed = metric_value(item, "elapsed_ms")
        parts: list[str] = []
        if p50 is not None:
            parts.append(f"p50 {format_metric(p50)}")
        if p95 is not None:
            parts.append(f"p95 {format_metric(p95)}")
        if delta is not None:
            parts.append(f"delta {format_metric(delta)}%")
        elif elapsed is not None and not parts:
            parts.append(f"elapsed {format_metric(elapsed)}ms")
        if not parts:
            continue
        row = f"{label}: " + ", ".join(parts)
        if row not in seen:
            seen.add(row)
            rows.append(row)
    return rows


def metric_value(item: dict[str, Any], key: str) -> float | int | None:
    value = item.get(key)
    if isinstance(value, (int, float)):
        return value
    metrics = item.get("metrics")
    if isinstance(metrics, dict):
        nested = metrics.get(key)
        if isinstance(nested, (int, float)):
            return nested
        elapsed = metrics.get("elapsed_ms")
        if isinstance(elapsed, dict):
            nested = elapsed.get(key)
            if isinstance(nested, (int, float)):
                return nested
    return None


def format_metric(value: float | int) -> str:
    return f"{value:.2f}" if isinstance(value, float) and not value.is_integer() else str(int(value))


def summarize_test_failure(item: dict[str, Any], idx: int) -> str:
    name = str(item.get("name", "unknown"))
    detail = str(item.get("detail", item.get("message", ""))).strip()
    location = str(item.get("location", item.get("file", ""))).strip()
    parts = [f"{idx}. {name}"]
    if detail:
        parts.append(detail)
    if location:
        parts.append(location)
    return " — ".join(parts)


def markdown_lint(data: dict[str, Any], lines: list[str]) -> None:
    if data.get("summary"):
        lines.append(f"- Lint summary: **{data['summary']}**")
    if data.get("phpcs_summary"):
        lines.append(f"- PHPCS: {data['phpcs_summary']}")
    if data.get("phpstan_summary"):
        lines.append(f"- PHPStan: {data['phpstan_summary']}")
    if data.get("build_failed"):
        lines.append(f"- Build failed: {data['build_failed']}")
    top_violations = [str(v) for v in (data.get("top_violations", []) or [])][:10]
    append_details(lines, "Top lint violations", top_violations)


def markdown_test(data: dict[str, Any], lines: list[str]) -> None:
    test_counts = data.get("test_counts", {}) or {}
    failed_tests = data.get("failed_tests", []) or []
    failed_count = int(test_counts.get("failed", 0) or 0) + int(test_counts.get("errors", 0) or 0)
    if not failed_count:
        failed_count = len(failed_tests)

    if failed_count > 0:
        lines.append(f"- Failed tests: **{failed_count}**")
    else:
        lines.append("- Test command failed, but structured output reported **0 failed test cases**.")
        status = str(data.get("status") or "").strip()
        exit_code = data.get("exit_code")
        if status or exit_code is not None:
            status_label = status or "unknown"
            exit_label = "unknown" if exit_code is None else str(exit_code)
            lines.append(f"- Runner status: `{status_label}` (exit code `{exit_label}`)")
        failure = str(data.get("failure") or "").strip()
        if failure:
            lines.append(f"- Failure: {failure}")
        lines.append("- Interpret this as a runner/tooling failure, not failed test assertions.")

    details = []
    for idx, item in enumerate(failed_tests[:10], start=1):
        if isinstance(item, dict):
            details.append(summarize_test_failure(item, idx))
        else:
            details.append(f"{idx}. {item}")
    append_details(lines, f"Failed test details ({min(len(details), 10)} shown)", details)


def normalize_file(raw: Any) -> str:
    if isinstance(raw, dict):
        return str(raw.get("path") or raw.get("file") or "unknown")
    return str(raw or "unknown")


def markdown_audit(data: dict[str, Any], lines: list[str]) -> None:
    baseline = data.get("baseline_comparison", {}) or {}
    summary = data.get("summary", {}) or {}
    findings = data.get("findings", []) if isinstance(data, dict) else []
    conventions = data.get("conventions", []) if isinstance(data, dict) else []

    new_items = baseline.get("new_items", []) if isinstance(baseline, dict) else []
    if isinstance(new_items, list) and new_items:
        lines.append(f":x: **{len(new_items)} new finding(s) on this PR:**")
        lines.append("")
        for idx, item in enumerate(new_items[:5], start=1):
            if isinstance(item, dict):
                lines.append(f"{idx}. {format_new_audit_item(item)}")
        if len(new_items) > 5:
            lines.append(f"... and {len(new_items) - 5} more")
        lines.append("")
        lines.append("_Full audit state below._")

    alignment = summary.get("alignment_score") if isinstance(summary, dict) else None
    if isinstance(alignment, (int, float)):
        lines.append(f"- Alignment score: **{alignment:.3f}**")

    outliers = summary.get("outliers_found") if isinstance(summary, dict) else None
    if isinstance(outliers, int):
        lines.append(f"- Outliers in current run: **{outliers}**")

    drift = baseline.get("drift_increased", False) if isinstance(baseline, dict) else False
    lines.append(f"- Drift increased: **{'yes' if drift else 'no'}**")

    # Collect outlier items from conventions
    outlier_items: list[dict[str, Any]] = []
    for conv in (conventions if isinstance(conventions, list) else []):
        if not isinstance(conv, dict):
            continue
        context_label = str(
            conv.get("context_label") or conv.get("name") or conv.get("rule") or "unknown"
        )
        for outlier in (conv.get("outliers", []) if isinstance(conv.get("outliers"), list) else []):
            if isinstance(outlier, dict):
                item = dict(outlier)
                item.setdefault("context_label", context_label)
                outlier_items.append(item)

    # Severity counts + top findings from findings + outliers
    severity_counts: dict[str, int] = {}
    top_findings: list[dict[str, str]] = []
    for item in (findings if isinstance(findings, list) else []) + outlier_items:
        if not isinstance(item, dict):
            continue
        severity = str(item.get("severity") or item.get("level") or "unknown").lower()
        severity_counts[severity] = severity_counts.get(severity, 0) + 1
        top_findings.append({
            "file": normalize_file(item.get("file") or item.get("path") or item.get("context_label")),
            "rule": str(item.get("rule") or item.get("kind") or item.get("category") or "outlier"),
            "message": str(item.get("description") or item.get("message") or "(outlier)"),
        })

    if severity_counts:
        known_counts = {k: v for k, v in severity_counts.items() if k != "unknown"}
        if known_counts:
            sev_text = ", ".join(f"{k}: {v}" for k, v in sorted(known_counts.items()))
            lines.append(f"- Severity counts: **{sev_text}**")

    if top_findings:
        detail_lines: list[str] = []
        for idx, finding in enumerate(top_findings[:10], start=1):
            file_value = finding["file"]
            rule_value = finding["rule"]
            message = finding["message"]
            detail = f"{idx}. **{file_value}** — {rule_value}"
            if message:
                detail += f" — {message}"
            detail_lines.append(detail)
        if len(top_findings) <= 5:
            lines.append("- Actionable findings:")
            for line in detail_lines:
                lines.append(f"  {line}")
        else:
            append_details(lines, f"Audit findings ({min(len(top_findings), 10)} shown)", detail_lines)


def markdown_refactor(data: dict[str, Any], lines: list[str]) -> None:
    files_modified = int(data.get("files_modified", 0) or 0)
    totals = data.get("plan_totals", {})
    total_fixes = int(totals.get("total_fixes_proposed", 0) or 0) if isinstance(totals, dict) else 0
    warnings = data.get("warnings", [])

    if total_fixes == 0 and files_modified == 0:
        lines.append("- No fixable changes")
        return

    lines.append(f"- Total fixes proposed: **{total_fixes}**")
    lines.append(f"- Files modified: **{files_modified}**")

    stages = data.get("stages", [])
    if isinstance(stages, list) and stages:
        lines.append("- Stages:")
        for stage in stages:
            if not isinstance(stage, dict):
                continue
            name = str(stage.get("stage", "unknown"))
            proposed = int(stage.get("fixes_proposed", 0) or 0)
            stage_files = int(stage.get("files_modified", 0) or 0)
            detected = stage.get("detected_findings")
            detected_str = f", {detected} findings detected" if detected is not None else ""
            lines.append(f"  - **{name}**: {proposed} fix(es), {stage_files} file(s){detected_str}")

    changed_files = data.get("changed_files", [])
    if isinstance(changed_files, list) and changed_files:
        detail_lines = [f"{idx}. `{f}`" for idx, f in enumerate(changed_files[:15], start=1)]
        if len(changed_files) > 15:
            detail_lines.append(f"... and {len(changed_files) - 15} more")
        append_details(lines, f"Changed files ({len(changed_files)})", detail_lines)

    if isinstance(warnings, list):
        notable = [str(w) for w in warnings if isinstance(w, str) and "merge order" not in w.lower()]
        if notable:
            append_details(lines, f"Warnings ({len(notable)})", notable[:10])


def markdown_bench(data: dict[str, Any], lines: list[str]) -> None:
    rows = collect_bench_metric_rows(data)
    scenarios = collect_scenario_names(data)
    regressions = count_matching_keys(data, {"regression", "regressions"})

    if scenarios:
        lines.append(f"- Benchmark scenarios: **{len(scenarios)}**")
    if regressions:
        lines.append(f"- Regression signals: **{regressions}**")
    if rows:
        lines.append("- Primary metrics:")
        for row in rows[:6]:
            lines.append(f"  - {row}")
        if len(rows) > 6:
            lines.append(f"  - ... and {len(rows) - 6} more")
    if not lines:
        lines.append("- Benchmark structured output is available in `homeboy-ci-results/bench.json`.")


# ── CLI entry point ───────────────────────────────────────────────────

def main() -> int:
    if len(sys.argv) < 3 or len(sys.argv) > 4:
        print(f"Usage: {sys.argv[0]} <command> <json-file> [compact|markdown]", file=sys.stderr)
        return 1

    raw_command = sys.argv[1].strip().lower()
    # Normalize compound commands like "refactor --from all" to base command
    command = raw_command.split()[0] if raw_command else raw_command
    path = sys.argv[2]
    mode = sys.argv[3].strip().lower() if len(sys.argv) == 4 else "markdown"

    data = load_json(path)
    if mode == "compact":
        print(compact_summary(command, data))
    else:
        print(markdown_summary(command, data))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
