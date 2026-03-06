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

from digest.parsers import extract_audit_digest, extract_lint_digest, extract_test_failures
from digest.render import render_markdown


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

    lint_log = read_text(os.path.join(output_dir, "lint.log"))
    test_log = read_text(os.path.join(output_dir, "test.log"))
    audit_log = read_text(os.path.join(output_dir, "audit.log"))

    lint_digest = extract_lint_digest(lint_log) if results.get("lint") == "fail" else {
        "lint_summary": "",
        "phpcs_summary": "",
        "phpstan_summary": "",
        "build_failed": "",
        "top_violations": [],
    }

    test_digest = extract_test_failures(test_log) if results.get("test") == "fail" else {
        "failed_tests_count": 0,
        "top_failed_tests": [],
    }
    audit_digest = extract_audit_digest(
        audit_log,
        extract_json_candidates,
        normalize_log_text,
    ) if results.get("audit") == "fail" else {
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
