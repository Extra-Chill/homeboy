#!/usr/bin/env python3
from __future__ import annotations

import json
import sys
from typing import Any


def load_json(path: str) -> dict[str, Any]:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    return data if isinstance(data, dict) else {}


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


def summarize_test_failure(item: dict[str, Any], idx: int) -> str:
    name = str(item.get("name", "unknown"))
    detail = str(item.get("detail", "")).strip()
    location = str(item.get("location", "")).strip()
    parts = [f"{idx}. {name}"]
    if detail:
        parts.append(detail)
    if location:
        parts.append(location)
    return " — ".join(parts)


def compact_summary(command: str, data: dict[str, Any]) -> str:
    if command == "lint":
        for value in [
            data.get("error_message"),
            data.get("build_failed"),
            data.get("lint_summary"),
            data.get("phpcs_summary"),
            data.get("phpstan_summary"),
        ]:
            if value:
                return str(value)
        return "structured lint details available"

    if command == "test":
        failed_count = int(data.get("failed_tests_count", 0) or 0)
        top_failed = data.get("top_failed_tests", []) or []
        if top_failed:
            first = top_failed[0] if isinstance(top_failed[0], dict) else {"name": str(top_failed[0])}
            detail = str(first.get("detail", "")).strip()
            name = str(first.get("name", "unknown")).strip()
            suffix = f" — {name}" if name else ""
            if detail:
                suffix += f": {detail}"
            return f"{failed_count} failed test(s){suffix}"
        return f"{failed_count} failed test(s)"

    if command == "audit":
        new_findings_count = int(data.get("new_findings_count", 0) or 0)
        if new_findings_count > 0:
            return f"{new_findings_count} new finding(s) since baseline"
        outliers = data.get("outliers_found")
        if isinstance(outliers, int):
            return f"{outliers} outlier(s) in current run"
        alignment = data.get("alignment_score")
        if isinstance(alignment, (int, float)):
            return f"alignment score {alignment:.3f}"
        return "structured audit details available"

    return "structured command details available"


def markdown_summary(command: str, data: dict[str, Any]) -> str:
    lines: list[str] = []

    if command == "lint":
        if data.get("lint_summary"):
            lines.append(f"- Lint summary: **{data['lint_summary']}**")
        if data.get("phpcs_summary"):
            lines.append(f"- PHPCS: {data['phpcs_summary']}")
        if data.get("phpstan_summary"):
            lines.append(f"- PHPStan: {data['phpstan_summary']}")
        if data.get("build_failed"):
            lines.append(f"- Build failed: {data['build_failed']}")
        if data.get("error_code"):
            lines.append(f"- Error code: `{data['error_code']}`")
        if data.get("error_message"):
            lines.append(f"- Error message: {data['error_message']}")
        if data.get("error_field"):
            lines.append(f"- Error field: `{data['error_field']}`")
        if data.get("error_hint"):
            lines.append(f"- Hint: {data['error_hint']}")
        top_violations = [str(v) for v in (data.get("top_violations", []) or [])][:10]
        append_details(lines, "Top lint violations", top_violations)

    elif command == "test":
        failed_count = int(data.get("failed_tests_count", 0) or 0)
        lines.append(f"- Failed tests: **{failed_count}**")
        top_failed = data.get("top_failed_tests", []) or []
        details = []
        for idx, item in enumerate(top_failed[:10], start=1):
            if isinstance(item, dict):
                details.append(summarize_test_failure(item, idx))
            else:
                details.append(f"{idx}. {item}")
        append_details(lines, f"Failed test details ({min(len(details), 10)} shown)", details)

    elif command == "audit":
        alignment = data.get("alignment_score")
        if isinstance(alignment, (int, float)):
            lines.append(f"- Alignment score: **{alignment:.3f}**")
        severity_counts = data.get("severity_counts", {}) or {}
        if severity_counts:
            sev_text = ", ".join(f"{k}: {v}" for k, v in sorted(severity_counts.items()))
            lines.append(f"- Severity counts: **{sev_text}**")
        outliers = data.get("outliers_found")
        if isinstance(outliers, int):
            lines.append(f"- Outliers in current run: **{outliers}**")
        lines.append(f"- Drift increased: **{'yes' if data.get('drift_increased') else 'no'}**")
        new_findings = data.get("new_findings", []) or []
        new_findings_count = int(data.get("new_findings_count", 0) or 0)
        if new_findings_count > 0:
            lines.append(f"- New findings since baseline: **{new_findings_count}**")
            for idx, finding in enumerate(new_findings[:5], start=1):
                context = str(finding.get("context", "unknown"))
                message = str(finding.get("message", ""))
                fingerprint = str(finding.get("fingerprint", ""))
                entry = f"  {idx}. **{context}**"
                if message:
                    entry += f" — {message}"
                if fingerprint:
                    entry += f" (`{fingerprint}`)"
                lines.append(entry)
        top_findings = data.get("top_findings", []) or []
        if top_findings:
            lines.append("- Top actionable findings:")
            detail_lines: list[str] = []
            for idx, finding in enumerate(top_findings[:10], start=1):
                file_value = str(finding.get("file", "unknown"))
                rule_value = str(finding.get("rule", "unknown"))
                message = str(finding.get("message", ""))
                line = f"  {idx}. **{file_value}** — {rule_value}"
                detail = f"{idx}. **{file_value}** — {rule_value}"
                if message:
                    line += f" — {message}"
                    detail += f" — {message}"
                lines.append(line)
                detail_lines.append(detail)
            append_details(lines, f"Audit findings ({min(len(top_findings), 10)} shown)", detail_lines)

    return "\n".join(lines)


def main() -> int:
    if len(sys.argv) < 3 or len(sys.argv) > 4:
        print(f"Usage: {sys.argv[0]} <command> <json-file> [compact|markdown]", file=sys.stderr)
        return 1

    command = sys.argv[1].strip().lower()
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
