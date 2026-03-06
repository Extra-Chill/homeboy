from __future__ import annotations

import re
from typing import Any


def _clean_excerpt_lines(lines: list[str], limit: int = 40) -> list[str]:
    cleaned: list[str] = []
    blank_run = 0
    for raw in lines:
        line = raw.rstrip()
        if not line.strip():
            blank_run += 1
            if blank_run > 1:
                continue
            cleaned.append("")
            continue
        blank_run = 0
        cleaned.append(line)
        if len(cleaned) >= limit:
            break
    while cleaned and cleaned[0] == "":
        cleaned.pop(0)
    while cleaned and cleaned[-1] == "":
        cleaned.pop()
    return cleaned


def _extract_excerpt_around_pattern(
    log_text: str,
    patterns: list[str],
    before: int = 2,
    after: int = 10,
    limit: int = 40,
) -> list[str]:
    lines = log_text.splitlines()
    for idx, raw in enumerate(lines):
        for pattern in patterns:
            if re.search(pattern, raw):
                start = max(0, idx - before)
                end = min(len(lines), idx + after + 1)
                return _clean_excerpt_lines(lines[start:end], limit=limit)
    return []


def extract_test_failures(log_text: str) -> dict[str, Any]:
    lines = log_text.splitlines()
    failures: list[dict[str, str]] = []

    for idx, line in enumerate(lines):
        m = re.match(r"^\d+\)\s+(.+)$", line.strip())
        if not m:
            continue
        test_name = m.group(1).strip()
        detail = ""
        location = ""
        for j in range(idx + 1, min(idx + 10, len(lines))):
            candidate = lines[j].strip()
            if not candidate:
                continue
            if not detail and (
                "Failed asserting" in candidate
                or "is not" in candidate
                or "Error:" in candidate
                or "TypeError" in candidate
                or "Exception" in candidate
            ):
                detail = candidate
            loc_match = re.search(r"\b(?:in|at)\s+([^\s]+:\d+)\b", candidate)
            if loc_match and not location:
                location = loc_match.group(1)
            if detail and location:
                break
        failures.append({"name": test_name, "detail": detail, "location": location})

    for idx, line in enumerate(lines):
        m = re.match(r"^----\s+(.+)\s+stdout\s+----$", line.strip())
        if not m:
            continue
        test_name = m.group(1).strip()
        detail = ""
        location = ""
        for j in range(idx + 1, min(idx + 12, len(lines))):
            candidate = lines[j].strip()
            if not candidate:
                continue
            if "panicked at" in candidate and not detail:
                detail = candidate
            loc_match = re.search(r"([^\s]+:\d+:\d+)", candidate)
            if loc_match and not location:
                location = loc_match.group(1)
            if detail and location:
                break
        failures.append({"name": test_name, "detail": detail, "location": location})

    deduped: list[dict[str, str]] = []
    seen: set[str] = set()
    for item in failures:
        key = f"{item.get('name','')}::{item.get('detail','')}::{item.get('location','')}"
        if key in seen:
            continue
        seen.add(key)
        deduped.append(item)

    failed_count = len(deduped)
    php_summary = re.search(
        r"Tests:\s*(\d+),\s*Assertions:\s*(\d+),\s*Failures:\s*(\d+)(?:,\s*Errors:\s*(\d+))?",
        log_text,
    )
    if php_summary:
        failures_num = int(php_summary.group(3) or 0)
        errors_num = int(php_summary.group(4) or 0)
        failed_count = max(failed_count, failures_num + errors_num)

    rust_summary = re.findall(
        r"test result:\s+FAILED\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;",
        log_text,
    )
    if rust_summary:
        failed_count = max(failed_count, sum(int(x[1]) for x in rust_summary))

    raw_excerpt = _extract_excerpt_around_pattern(
        log_text,
        [
            r"^\d+\)\s+",
            r"^----\s+.+\s+stdout\s+----$",
            r"Failed asserting",
            r"\bpanicked at\b",
            r"\bERROR!\b",
            r"\bFAILURES!\b",
            r"test result:\s+FAILED",
        ],
        before=1,
        after=18,
        limit=60,
    )

    return {
        "failed_tests_count": failed_count,
        "top_failed_tests": deduped[:10],
        "raw_excerpt": raw_excerpt,
    }


def extract_lint_digest(log_text: str) -> dict[str, Any]:
    lint_summary = ""
    phpcs_summary = ""
    phpstan_summary = ""
    build_failed = ""
    top_violations: list[str] = []
    error_code = ""
    error_message = ""
    error_field = ""
    error_hint = ""

    m = re.search(r"LINT SUMMARY:\s*(.+)", log_text)
    if m:
        lint_summary = m.group(1).strip()

    m = re.search(r"PHPCS SUMMARY:\s*(.+)", log_text)
    if m:
        phpcs_summary = m.group(1).strip()

    m = re.search(r"PHPSTAN SUMMARY:\s*(.+)", log_text)
    if m:
        phpstan_summary = m.group(1).strip()

    m = re.search(r"BUILD FAILED:\s*(.+)", log_text)
    if m:
        build_failed = m.group(1).strip()

    m = re.search(r'"code":\s*"([^"]+)"', log_text)
    if m:
        error_code = m.group(1).strip()

    m = re.search(r'"message":\s*"([^"]+)"', log_text)
    if m:
        error_message = m.group(1).strip()

    m = re.search(r'"field":\s*"([^"]+)"', log_text)
    if m:
        error_field = m.group(1).strip()

    m = re.search(r'"hints":\s*\[\s*\{\s*"message":\s*"([^"]+)"', log_text, re.S)
    if m:
        error_hint = m.group(1).strip()

    lines = log_text.splitlines()
    in_top = False
    for raw in lines:
        line = raw.rstrip("\n")
        if "TOP VIOLATIONS:" in line:
            in_top = True
            continue
        if not in_top:
            continue
        if not line.strip():
            break
        if line.startswith(" ") or line.startswith("\t"):
            top_violations.append(line.strip())

    raw_excerpt = _extract_excerpt_around_pattern(
        log_text,
        [
            r'"success":\s*false',
            r'"code":\s*"',
            r"LINT SUMMARY:",
            r"PHPCS SUMMARY:",
            r"PHPSTAN SUMMARY:",
            r"BUILD FAILED:",
            r"PHP Fatal error:",
            r"^error\[",
        ],
        before=1,
        after=18,
        limit=60,
    )

    return {
        "lint_summary": lint_summary,
        "phpcs_summary": phpcs_summary,
        "phpstan_summary": phpstan_summary,
        "build_failed": build_failed,
        "error_code": error_code,
        "error_message": error_message,
        "error_field": error_field,
        "error_hint": error_hint,
        "top_violations": top_violations[:10],
        "raw_excerpt": raw_excerpt,
    }


def extract_audit_digest(
    log_text: str,
    extract_json_candidates,
    normalize_log_text,
) -> dict[str, Any]:
    payloads = [
        p
        for p in extract_json_candidates(normalize_log_text(log_text))
        if isinstance(p.get("data"), dict)
    ]
    if not payloads:
        return {
            "drift_increased": False,
            "outliers_found": None,
            "severity_counts": {},
            "top_findings": [],
            "raw_excerpt": _extract_excerpt_around_pattern(log_text, [r'.+'], before=0, after=20, limit=40),
        }

    payload = payloads[-1]
    data = payload.get("data", {})
    baseline = data.get("baseline_comparison", {}) if isinstance(data, dict) else {}
    summary = data.get("summary", {}) if isinstance(data, dict) else {}
    new_items = baseline.get("new_items", []) if isinstance(baseline, dict) else []
    if not isinstance(new_items, list):
        new_items = []

    findings = data.get("findings", []) if isinstance(data, dict) else []
    if not isinstance(findings, list):
        findings = []

    conventions = data.get("conventions", []) if isinstance(data, dict) else []
    if not isinstance(conventions, list):
        conventions = []

    def normalize_file_value(raw: Any, fallback: str = "unknown") -> str:
        file_value = raw
        if isinstance(file_value, dict):
            file_value = file_value.get("path") or file_value.get("file")
        return str(file_value or fallback)

    def normalize_rule_value(item: dict[str, Any], fallback: str = "outlier") -> str:
        return str(
            item.get("rule")
            or item.get("kind")
            or item.get("category")
            or item.get("type")
            or item.get("status")
            or fallback
        )

    def normalize_message_value(item: dict[str, Any], fallback: str = "(outlier)") -> str:
        message = item.get("description") or item.get("message")
        if not message:
            message = item.get("expected_namespace") or item.get("expected_pattern") or fallback
        return str(message)

    outlier_items: list[dict[str, Any]] = []
    for conv in conventions:
        if not isinstance(conv, dict):
            continue
        context_label = str(
            conv.get("context_label")
            or conv.get("name")
            or conv.get("rule")
            or conv.get("pattern")
            or "unknown"
        )
        outliers = conv.get("outliers", [])
        if not isinstance(outliers, list):
            continue
        for outlier in outliers:
            if not isinstance(outlier, dict):
                continue
            item = dict(outlier)
            item.setdefault("context_label", context_label)
            outlier_items.append(item)

    severity_counts: dict[str, int] = {}
    top_findings: list[dict[str, str]] = []
    new_findings: list[dict[str, str]] = []

    for item in new_items:
        if not isinstance(item, dict):
            continue
        new_findings.append(
            {
                "context": str(item.get("context_label") or item.get("file") or item.get("path") or "unknown"),
                "message": normalize_message_value(item, "(new finding)"),
                "fingerprint": str(item.get("fingerprint") or ""),
            }
        )

    source_items = findings + outlier_items

    for item in source_items:
        if not isinstance(item, dict):
            continue
        severity = str(item.get("severity") or item.get("level") or "unknown").lower()
        severity_counts[severity] = severity_counts.get(severity, 0) + 1

        top_findings.append(
            {
                "file": normalize_file_value(
                    item.get("file") or item.get("path") or item.get("context_label"),
                    str(item.get("context_label") or "unknown"),
                ),
                "rule": normalize_rule_value(item),
                "message": normalize_message_value(item),
                "suggested_fix": str(item.get("suggested_fix") or item.get("suggestion") or ""),
            }
        )

    raw_excerpt = _extract_excerpt_around_pattern(
        log_text,
        [
            r'"outliers_found"\s*:',
            r'"drift_increased"\s*:',
            r'"findings"\s*:',
            r'"baseline_comparison"\s*:',
            r'"success":\s*false',
            r'"error":\s*\{',
        ],
        before=1,
        after=30,
        limit=80,
    )

    return {
        "drift_increased": bool(baseline.get("drift_increased", False)),
        "new_findings_count": len(new_findings),
        "new_findings": new_findings[:100],
        "alignment_score": summary.get("alignment_score") if isinstance(summary, dict) else None,
        "outliers_found": summary.get("outliers_found") if isinstance(summary, dict) else None,
        "parsed_outlier_items": len(outlier_items),
        "severity_counts": severity_counts,
        "top_findings": top_findings[:500],
        "raw_excerpt": raw_excerpt,
    }
