#!/usr/bin/env python3
"""Build compact failure digests from Homeboy command logs.

Writes markdown + machine-readable JSON artifacts into HOMEBOY_OUTPUT_DIR.
"""

from __future__ import annotations

import json
import os
import sys
import subprocess
from typing import Any

# NOTE:
# This script is executed as a file path from composite actions, so Python sets
# sys.path to this directory (scripts/digest). Use local imports instead of
# package-qualified imports to avoid ModuleNotFoundError in CI.
from render import render_markdown


def parse_bool(value: str | None) -> bool:
    if value is None:
        return False
    return value.strip().lower() in {"1", "true", "yes", "y", "on"}


def derive_fixable_commands(
    commands_csv: str,
    autofix_commands_csv: str,
) -> set[str]:
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
    potential_fixable_candidates = derive_fixable_commands(commands_csv, autofix_commands_csv)
    fixable_candidates = potential_fixable_candidates if autofix_enabled else set()

    auto_fixable_failed: list[str] = []
    potential_auto_fixable_failed: list[str] = []
    human_needed_failed: list[str] = []

    for cmd in failed_commands:
        normalized = cmd.strip().lower()
        if normalized in potential_fixable_candidates:
            potential_auto_fixable_failed.append(cmd)

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
        "potential_fixable_candidates": sorted(potential_fixable_candidates),
        "failed_commands": failed_commands,
        "auto_fixable_failed_commands": auto_fixable_failed,
        "potential_auto_fixable_failed_commands": potential_auto_fixable_failed,
        "human_needed_failed_commands": human_needed_failed,
        "overall": overall,
    }


def read_json(path: str) -> dict[str, Any] | None:
    """Read a JSON file, returning None if missing or invalid."""
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
            return data if isinstance(data, dict) else None
    except (OSError, json.JSONDecodeError):
        return None


def build_lint_digest_from_json(payload: dict[str, Any]) -> dict[str, Any]:
    """Build lint digest from structured JSON output."""
    data = payload.get("data", {})
    error = payload.get("error", {})

    return {
        "lint_summary": str(data.get("summary", "")),
        "phpcs_summary": str(data.get("phpcs_summary", "")),
        "phpstan_summary": str(data.get("phpstan_summary", "")),
        "build_failed": str(data.get("build_failed", "")),
        "error_code": str(error.get("code", "")),
        "error_message": str(error.get("message", "")),
        "error_field": str(error.get("details", {}).get("field", "")),
        "error_hint": "",
        "top_violations": [str(v) for v in data.get("top_violations", [])][:10],
        "raw_excerpt": [],
    }


def read_log_excerpt(output_dir: str, stem: str, max_lines: int = 25) -> list[str]:
    """Return a compact tail excerpt from a command log."""
    path = os.path.join(output_dir, f"{stem}.log")
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            lines = [line.rstrip() for line in f]
    except OSError:
        return []

    compact = [line for line in lines if line.strip()]
    return compact[-max_lines:]


def build_test_digest_from_json(payload: dict[str, Any], raw_excerpt: list[str] | None = None) -> dict[str, Any]:
    """Build test digest from structured JSON output."""
    data = payload.get("data", {})
    test_counts = data.get("test_counts", {})
    failed_tests = data.get("failed_tests", [])

    top_failed = []
    for t in failed_tests[:10]:
        if isinstance(t, dict):
            top_failed.append({
                "name": str(t.get("name", "")),
                "detail": str(t.get("detail", t.get("message", ""))),
                "location": str(t.get("location", t.get("file", ""))),
            })
        else:
            top_failed.append({"name": str(t), "detail": "", "location": ""})

    failed_count = int(test_counts.get("failed", 0)) + int(test_counts.get("errors", 0))
    if not failed_count:
        failed_count = len(top_failed)

    return {
        "failed_tests_count": failed_count,
        "top_failed_tests": top_failed,
        "raw_excerpt": raw_excerpt or [],
        "status": str(data.get("status", "")),
        "exit_code": data.get("exit_code"),
        "failure_message": str(data.get("failure") or ""),
    }


def build_audit_digest_from_json(payload: dict[str, Any]) -> dict[str, Any]:
    """Build audit digest from structured JSON output.

    The JSON envelope has the same structure that extract_audit_digest()
    scraped from logs — baseline_comparison, summary, findings, conventions.
    """
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
        new_findings.append({
            "context": str(item.get("context_label") or item.get("file") or "unknown"),
            "message": str(item.get("description") or item.get("message") or "(new finding)"),
            "fingerprint": str(item.get("fingerprint") or ""),
        })

    def normalize_file(raw: Any) -> str:
        if isinstance(raw, dict):
            return str(raw.get("path") or raw.get("file") or "unknown")
        return str(raw or "unknown")

    for item in findings + outlier_items:
        if not isinstance(item, dict):
            continue
        severity = str(item.get("severity") or item.get("level") or "unknown").lower()
        severity_counts[severity] = severity_counts.get(severity, 0) + 1
        top_findings.append({
            "file": normalize_file(item.get("file") or item.get("path") or item.get("context_label")),
            "rule": str(item.get("rule") or item.get("kind") or item.get("category") or "outlier"),
            "message": str(item.get("description") or item.get("message") or "(outlier)"),
            "suggested_fix": str(item.get("suggested_fix") or item.get("suggestion") or ""),
        })

    return {
        "drift_increased": bool(baseline.get("drift_increased", False)),
        "new_findings_count": len(new_findings),
        "new_findings": new_findings[:100],
        "alignment_score": summary.get("alignment_score") if isinstance(summary, dict) else None,
        "outliers_found": summary.get("outliers_found") if isinstance(summary, dict) else None,
        "parsed_outlier_items": len(outlier_items),
        "severity_counts": severity_counts,
        "top_findings": top_findings[:500],
        "raw_excerpt": [],
    }


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

    # Structured JSON files extracted from homeboy output by run-homeboy-commands.sh.
    # These are the canonical action-side contract for downstream rendering.
    lint_json = read_json(os.path.join(output_dir, "lint.json"))
    test_json = read_json(os.path.join(output_dir, "test.json"))
    audit_json = read_json(os.path.join(output_dir, "audit.json"))

    if results.get("lint") == "fail":
        if lint_json and lint_json.get("data"):
            lint_digest = build_lint_digest_from_json(lint_json)
        else:
            lint_digest = {
                "lint_summary": "",
                "phpcs_summary": "",
                "phpstan_summary": "",
                "build_failed": "",
                "error_code": "",
                "error_message": "",
                "error_field": "",
                "error_hint": "",
                "top_violations": [],
                "raw_excerpt": [],
            }
    else:
        lint_digest = {
            "lint_summary": "",
            "phpcs_summary": "",
            "phpstan_summary": "",
            "build_failed": "",
            "top_violations": [],
        }

    if results.get("test") == "fail":
        if test_json and test_json.get("data"):
            test_digest = build_test_digest_from_json(test_json, read_log_excerpt(output_dir, "test"))
        else:
            test_digest = {
                "failed_tests_count": 0,
                "top_failed_tests": [],
                "raw_excerpt": read_log_excerpt(output_dir, "test"),
                "status": "",
                "exit_code": None,
                "failure_message": "",
            }
    else:
        test_digest = {
            "failed_tests_count": 0,
            "top_failed_tests": [],
        }

    if results.get("audit") == "fail":
        if audit_json and audit_json.get("data"):
            audit_digest = build_audit_digest_from_json(audit_json)
        else:
            audit_digest = {
                "drift_increased": False,
                "new_findings_count": 0,
                "new_findings": [],
                "alignment_score": None,
                "outliers_found": None,
                "parsed_outlier_items": 0,
                "severity_counts": {},
                "top_findings": [],
                "raw_excerpt": [],
            }
    else:
        audit_digest = {
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

    md_path = os.path.join(output_dir, "homeboy-failure-digest.md")

    job_links = resolve_failed_job_links(run_url)
    markdown = render_markdown(
        lint_digest,
        test_digest,
        audit_digest,
        autofixability,
        run_url,
        tooling,
        job_links,
        results,
    )
    with open(md_path, "w", encoding="utf-8") as f:
        f.write(markdown)
        f.write("\n")

    print(md_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
