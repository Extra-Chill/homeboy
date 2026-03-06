#!/usr/bin/env python3
"""Render actionable markdown from Homeboy audit JSON output in a log file."""

from __future__ import annotations

import json
import sys
from json import JSONDecoder


def extract_json_candidates(text: str) -> list[dict]:
    decoder = JSONDecoder()
    out: list[dict] = []
    i = 0

    while i < len(text):
        if text[i] != "{":
            i += 1
            continue

        try:
            obj, end = decoder.raw_decode(text[i:])
        except json.JSONDecodeError:
            i += 1
            continue

        if isinstance(obj, dict):
            out.append(obj)

        i += max(end, 1)

    return out


def normalize_log_text(raw_text: str) -> str:
    normalized_lines: list[str] = []
    for line in raw_text.splitlines():
        if "Z " in line:
            normalized_lines.append(line.rsplit("Z ", 1)[1])
        else:
            normalized_lines.append(line)
    return "\n".join(normalized_lines)


def is_audit_payload(obj: dict) -> bool:
    data = obj.get("data")
    if not isinstance(data, dict):
        return False

    if "baseline_comparison" in data or "summary" in data or "findings" in data:
        return True

    command = str(data.get("command", ""))
    return command.startswith("audit")


def normalize_file_value(raw: object, fallback: str = "unknown") -> str:
    if isinstance(raw, dict):
        raw = raw.get("path") or raw.get("file")
    return str(raw or fallback)


def normalize_rule_value(item: dict, fallback: str = "outlier") -> str:
    return str(
        item.get("rule")
        or item.get("kind")
        or item.get("category")
        or item.get("type")
        or item.get("status")
        or fallback
    )


def normalize_message_value(item: dict, fallback: str = "(outlier)") -> str:
    message = item.get("description") or item.get("message")
    if not message:
        message = item.get("expected_namespace") or item.get("expected_pattern") or fallback
    return str(message)


def build_audit_summary(log_text: str) -> dict:
    payloads = [
        p for p in extract_json_candidates(normalize_log_text(log_text)) if is_audit_payload(p)
    ]
    if not payloads:
        return {}

    payload = payloads[-1]
    data = payload.get("data", {}) if isinstance(payload, dict) else {}
    baseline = data.get("baseline_comparison", {}) if isinstance(data, dict) else {}
    summary = data.get("summary", {}) if isinstance(data, dict) else {}
    findings = data.get("findings", []) if isinstance(data, dict) else []
    conventions = data.get("conventions", []) if isinstance(data, dict) else []
    new_items = baseline.get("new_items", []) if isinstance(baseline, dict) else []

    if not isinstance(findings, list):
        findings = []
    if not isinstance(conventions, list):
        conventions = []
    if not isinstance(new_items, list):
        new_items = []

    outlier_items: list[dict] = []
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
    for item in findings + outlier_items:
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

    return {
        "alignment_score": summary.get("alignment_score") if isinstance(summary, dict) else None,
        "drift_increased": bool(baseline.get("drift_increased", False)),
        "outliers_found": summary.get("outliers_found") if isinstance(summary, dict) else None,
        "parsed_outlier_items": len(outlier_items),
        "severity_counts": severity_counts,
        "new_findings": new_findings[:100],
        "top_findings": top_findings[:100],
    }


def main() -> int:
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <audit-log-file>", file=sys.stderr)
        return 1

    log_path = sys.argv[1]
    try:
        with open(log_path, "r", encoding="utf-8", errors="replace") as f:
            text = f.read()
    except OSError:
        return 1

    summary = build_audit_summary(text)
    if not summary:
        return 2

    alignment_score = summary.get("alignment_score")
    if isinstance(alignment_score, (int, float)):
        print(f"- Alignment score: **{alignment_score:.3f}**")

    severity_counts = summary.get("severity_counts", {}) or {}
    if severity_counts:
        sev_text = ", ".join(f"{k}: {v}" for k, v in sorted(severity_counts.items()))
        print(f"- Severity counts: **{sev_text}**")

    print(f"- Drift increased: **{'yes' if summary.get('drift_increased') else 'no'}**")

    outliers_found = summary.get("outliers_found")
    if isinstance(outliers_found, int):
        print(f"- Outliers in current run: **{outliers_found}**")

    parsed_outlier_items = summary.get("parsed_outlier_items")
    if isinstance(parsed_outlier_items, int) and parsed_outlier_items > 0:
        print(f"- Parsed outlier entries: **{parsed_outlier_items}**")

    new_findings = summary.get("new_findings", []) or []
    if new_findings:
        print(f"- New findings since baseline: **{len(new_findings)}**")
        for idx, item in enumerate(new_findings[:5], start=1):
            context = str(item.get("context", "unknown"))
            message = str(item.get("message", ""))
            fingerprint = str(item.get("fingerprint", ""))
            line = f"  {idx}. **{context}**"
            if message:
                line += f" — {message}"
            if fingerprint:
                line += f" (`{fingerprint}`)"
            print(line)

    top_findings = summary.get("top_findings", []) or []
    if top_findings:
        print("- Top actionable findings:")
        for idx, item in enumerate(top_findings[:5], start=1):
            line = f"  {idx}. **{item.get('file', 'unknown')}** — {item.get('rule', 'unknown')}"
            message = str(item.get("message", ""))
            if message:
                line += f" — {message}"
            print(line)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
