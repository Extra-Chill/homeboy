#!/usr/bin/env python3
"""Build compact failure digests from Homeboy command logs.

Writes markdown + machine-readable JSON artifacts into HOMEBOY_OUTPUT_DIR.
"""

from __future__ import annotations

import json
import os
import re
import sys
import subprocess
from json import JSONDecoder
from typing import Any


def read_text(path: str) -> str:
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def extract_json_candidates(text: str) -> list[dict[str, Any]]:
    decoder = JSONDecoder()
    out: list[dict[str, Any]] = []
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

    return {
        "failed_tests_count": failed_count,
        "top_failed_tests": deduped[:10],
    }


def extract_audit_digest(log_text: str) -> dict[str, Any]:
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

    source_items = new_items if new_items else findings
    severity_counts: dict[str, int] = {}
    top_findings: list[dict[str, str]] = []

    for item in source_items:
        if not isinstance(item, dict):
            continue
        severity = str(item.get("severity") or item.get("level") or "unknown").lower()
        severity_counts[severity] = severity_counts.get(severity, 0) + 1

        top_findings.append(
            {
                "file": str(item.get("file") or item.get("path") or item.get("context_label") or "unknown"),
                "rule": str(item.get("rule") or item.get("category") or item.get("type") or "unknown"),
                "message": str(item.get("description") or item.get("message") or "(no description)"),
                "suggested_fix": str(item.get("suggested_fix") or item.get("suggestion") or ""),
            }
        )

    return {
        "drift_increased": bool(baseline.get("drift_increased", False)),
        "outliers_found": summary.get("outliers_found") if isinstance(summary, dict) else None,
        "severity_counts": severity_counts,
        "top_findings": top_findings[:10],
    }


def parse_bool(value: str | None) -> bool:
    if value is None:
        return False
    return value.strip().lower() in {"1", "true", "yes", "y", "on"}


def derive_fixable_commands(
    commands_csv: str,
    autofix_enabled: bool,
    autofix_commands_csv: str,
) -> set[str]:
    if not autofix_enabled:
        return set()

    if autofix_commands_csv.strip():
        out: set[str] = set()
        for raw in autofix_commands_csv.split(","):
            token = raw.strip().split(" ", 1)[0].strip().lower()
            if token:
                out.add(token)
        return out

    defaults: set[str] = set()
    for raw in commands_csv.split(","):
        cmd = raw.strip().lower()
        if cmd in {"lint", "test", "audit"}:
            defaults.add(cmd)
    return defaults


def classify_autofixability(
    results: dict[str, Any],
    commands_csv: str,
    autofix_enabled: bool,
    autofix_attempted: bool,
    autofix_commands_csv: str,
) -> dict[str, Any]:
    failed_commands = sorted(
        [str(cmd) for cmd, status in results.items() if isinstance(cmd, str) and status == "fail"]
    )
    fixable_candidates = derive_fixable_commands(
        commands_csv, autofix_enabled, autofix_commands_csv
    )

    auto_fixable_failed: list[str] = []
    human_needed_failed: list[str] = []

    for cmd in failed_commands:
        normalized = cmd.strip().lower()
        if normalized in fixable_candidates and not autofix_attempted:
            auto_fixable_failed.append(cmd)
        else:
            human_needed_failed.append(cmd)

    if failed_commands and auto_fixable_failed and not human_needed_failed:
        overall = "auto_fixable"
    elif failed_commands and auto_fixable_failed and human_needed_failed:
        overall = "mixed"
    elif failed_commands:
        overall = "human_needed"
    else:
        overall = "none"

    return {
        "autofix_enabled": autofix_enabled,
        "autofix_attempted": autofix_attempted,
        "fixable_candidates": sorted(fixable_candidates),
        "failed_commands": failed_commands,
        "auto_fixable_failed_commands": auto_fixable_failed,
        "human_needed_failed_commands": human_needed_failed,
        "overall": overall,
    }


def render_markdown(
    test_digest: dict[str, Any],
    audit_digest: dict[str, Any],
    autofixability: dict[str, Any],
    run_url: str,
    tooling: dict[str, str],
) -> str:
    lines: list[str] = []
    lines.append("## Failure Digest")
    lines.append("")

    lines.append("### Tooling versions")
    lines.append(f"- Homeboy CLI: **{tooling.get('homeboy_cli_version', 'unknown')}**")
    lines.append(
        "- Extension: "
        f"**{tooling.get('extension_id', 'auto')}** from "
        f"`{tooling.get('extension_source', 'auto')}`"
    )
    lines.append(
        f"- Extension revision: `{tooling.get('extension_revision', 'unknown')}`"
    )
    lines.append(
        "- Action: "
        f"`{tooling.get('action_repository', 'unknown')}@{tooling.get('action_ref', 'unknown')}`"
    )
    lines.append("")

    lines.append("### Test Failure Digest")
    lines.append(f"- Failed tests: **{test_digest.get('failed_tests_count', 0)}**")
    top_tests = test_digest.get("top_failed_tests", []) or []
    if top_tests:
        lines.append("- Top failed tests:")
        for idx, test in enumerate(top_tests[:5], start=1):
            name = test.get("name", "unknown")
            detail = test.get("detail", "")
            location = test.get("location", "")
            parts = [f"{idx}. **{name}**"]
            if detail:
                parts.append(detail)
            if location:
                parts.append(f"`{location}`")
            lines.append("  " + " — ".join(parts))
    else:
        lines.append("- No per-test failure details parsed from test log.")
    lines.append(f"- Full test log: {run_url}")
    lines.append("")

    lines.append("### Audit Failure Digest")
    severity_counts = audit_digest.get("severity_counts", {}) or {}
    if severity_counts:
        sev_text = ", ".join(f"{k}: {v}" for k, v in sorted(severity_counts.items()))
        lines.append(f"- Severity counts: **{sev_text}**")
    outliers = audit_digest.get("outliers_found")
    if isinstance(outliers, int):
        lines.append(f"- Outliers in current run: **{outliers}**")
    lines.append(f"- Drift increased: **{'yes' if audit_digest.get('drift_increased') else 'no'}**")

    top_findings = audit_digest.get("top_findings", []) or []
    if top_findings:
        lines.append("- Top actionable findings:")
        for idx, finding in enumerate(top_findings[:5], start=1):
            line = (
                f"  {idx}. **{finding.get('file','unknown')}** — "
                f"{finding.get('rule','unknown')} — {finding.get('message','')}"
            )
            lines.append(line)
    else:
        lines.append("- No structured audit findings parsed from audit log.")
    lines.append(f"- Full audit log: {run_url}")
    lines.append("")

    lines.append("### Autofixability classification")
    lines.append(f"- Overall: **{autofixability.get('overall', 'unknown')}**")
    lines.append(
        f"- Autofix enabled: **{'yes' if autofixability.get('autofix_enabled') else 'no'}**"
    )
    lines.append(
        f"- Autofix attempted this run: **{'yes' if autofixability.get('autofix_attempted') else 'no'}**"
    )

    fixable = autofixability.get("auto_fixable_failed_commands", []) or []
    human = autofixability.get("human_needed_failed_commands", []) or []
    if fixable:
        lines.append("- Auto-fixable failed commands:")
        for cmd in fixable:
            lines.append(f"  - `{cmd}`")
    if human:
        lines.append("- Human-needed failed commands:")
        for cmd in human:
            lines.append(f"  - `{cmd}`")
    if not fixable and not human:
        lines.append("- No failed commands to classify.")
    lines.append("")

    lines.append("### Machine-readable artifacts")
    lines.append("- `homeboy-test-failures.json`")
    lines.append("- `homeboy-audit-summary.json`")
    lines.append("- `homeboy-autofixability.json`")

    return "\n".join(lines)


def write_json(path: str, payload: dict[str, Any]) -> None:
    with open(path, "w", encoding="utf-8") as f:
        json.dump(payload, f, indent=2)
        f.write("\n")


def resolve_failed_job_links(run_url: str) -> dict[str, str]:
    """Resolve direct failed job URLs for this workflow run via gh api."""
    run_id = run_url.rstrip("/").split("/")[-1]
    if not run_id.isdigit():
        return {}

    repo = os.environ.get("GITHUB_REPOSITORY", "")
    if not repo:
        return {}

    cmd = [
        "gh",
        "api",
        f"repos/{repo}/actions/runs/{run_id}/jobs",
        "--jq",
        ".jobs[] | select(.conclusion == \"failure\") | [.name, .html_url] | @tsv",
    ]

    try:
        output = subprocess.check_output(cmd, text=True, stderr=subprocess.DEVNULL)
    except Exception:
        return {}

    links: dict[str, str] = {}
    for raw in output.splitlines():
        parts = raw.split("\t", 1)
        if len(parts) != 2:
            continue
        name, url = parts[0].strip(), parts[1].strip()
        if name and url:
            links[name] = url
    return links


def main() -> int:
    if len(sys.argv) != 14:
        print(
            f"Usage: {sys.argv[0]} "
            "<output_dir> <results_json> <run_url> "
            "<homeboy_cli_version> <extension_id> <extension_source> "
            "<extension_revision> <action_repository> <action_ref> "
            "<commands_csv> <autofix_enabled> <autofix_attempted> <autofix_commands_csv>",
            file=sys.stderr,
        )
        return 1

    (
        output_dir,
        results_raw,
        run_url,
        homeboy_cli_version,
        extension_id,
        extension_source,
        extension_revision,
        action_repository,
        action_ref,
        commands_csv,
        autofix_enabled_raw,
        autofix_attempted_raw,
        autofix_commands_csv,
    ) = sys.argv[1:14]
    os.makedirs(output_dir, exist_ok=True)

    tooling = {
        "homeboy_cli_version": homeboy_cli_version,
        "extension_id": extension_id,
        "extension_source": extension_source,
        "extension_revision": extension_revision,
        "action_repository": action_repository,
        "action_ref": action_ref,
    }

    try:
        results = json.loads(results_raw) if results_raw else {}
    except json.JSONDecodeError:
        results = {}

    autofix_enabled = parse_bool(autofix_enabled_raw)
    autofix_attempted = parse_bool(autofix_attempted_raw)

    test_log = read_text(os.path.join(output_dir, "test.log"))
    audit_log = read_text(os.path.join(output_dir, "audit.log"))

    test_digest = extract_test_failures(test_log) if results.get("test") == "fail" else {
        "failed_tests_count": 0,
        "top_failed_tests": [],
    }
    audit_digest = extract_audit_digest(audit_log) if results.get("audit") == "fail" else {
        "drift_increased": False,
        "outliers_found": None,
        "severity_counts": {},
        "top_findings": [],
    }
    autofixability = classify_autofixability(
        results,
        commands_csv,
        autofix_enabled,
        autofix_attempted,
        autofix_commands_csv,
    )

    test_json_path = os.path.join(output_dir, "homeboy-test-failures.json")
    audit_json_path = os.path.join(output_dir, "homeboy-audit-summary.json")
    autofixability_json_path = os.path.join(output_dir, "homeboy-autofixability.json")
    md_path = os.path.join(output_dir, "homeboy-failure-digest.md")

    write_json(test_json_path, test_digest)
    write_json(audit_json_path, audit_digest)
    write_json(autofixability_json_path, autofixability)

    markdown = render_markdown(test_digest, audit_digest, autofixability, run_url, tooling)
    job_links = resolve_failed_job_links(run_url)
    if job_links:
        extra = ["", "### Failed job links"]
        if "Build & Test" in job_links:
            extra.append(f"- Build & Test (failed job): {job_links['Build & Test']}")
        if "Homeboy Audit" in job_links:
            extra.append(f"- Homeboy Audit (failed job): {job_links['Homeboy Audit']}")
        markdown = markdown + "\n" + "\n".join(extra)
    with open(md_path, "w", encoding="utf-8") as f:
        f.write(markdown)
        f.write("\n")

    print(md_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
