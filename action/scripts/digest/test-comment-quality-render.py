#!/usr/bin/env python3
"""Smoke tests for PR comment signal/noise rendering."""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "scripts/digest/fixtures"
RENDERER = ROOT / "scripts/digest/render-command-summary.py"
sys.path.insert(0, str(ROOT / "scripts/digest"))

from render import render_markdown  # noqa: E402


def assert_equal(actual: str, expected: str, label: str) -> None:
    if actual != expected:
        raise AssertionError(f"{label} mismatch\n--- actual ---\n{actual}\n--- expected ---\n{expected}")


def assert_contains(text: str, needle: str, label: str) -> None:
    if needle not in text:
        raise AssertionError(f"missing {label}: {needle!r}\n---\n{text}")


def assert_not_contains(text: str, needle: str, label: str) -> None:
    if needle in text:
        raise AssertionError(f"unexpected {label}: {needle!r}\n---\n{text}")


def render(command: str, fixture: str) -> str:
    return subprocess.check_output(
        [sys.executable, str(RENDERER), command, str(FIXTURES / fixture), "markdown"],
        text=True,
    ).strip()


def main() -> int:
    audit = render("audit", "audit-comment-quality-input.json")
    expected_audit = (FIXTURES / "audit-comment-quality-expected.md").read_text(encoding="utf-8").strip()
    assert_equal(audit, expected_audit, "audit markdown")
    assert_not_contains(audit, "Top actionable findings", "duplicated top findings heading")
    assert_not_contains(audit, "unknown:", "unknown severity count")
    assert_contains(audit, ":x: **3 new finding(s) on this PR:**", "promoted delta block")

    refactor = render("refactor --from all", "refactor-noop-input.json")
    expected_refactor = (FIXTURES / "refactor-noop-expected.md").read_text(encoding="utf-8").strip()
    assert_equal(refactor, expected_refactor, "refactor no-op markdown")
    assert_not_contains(refactor, "Warnings", "no-op warnings detail")
    assert_not_contains(refactor, "Stages", "no-op stage listing")

    tooling_failure = render("test", "test-tooling-failure-input.json")
    assert_contains(
        tooling_failure,
        "Test command failed, but structured output reported **0 failed test cases**.",
        "zero-failed test command explanation",
    )
    assert_contains(tooling_failure, "Runner status: `failed` (exit code `1`)", "test runner status")
    assert_contains(tooling_failure, "runner/tooling failure", "test failure classification")
    assert_not_contains(tooling_failure, "Failed tests: **0**", "misleading zero failed tests line")

    full_digest = render_markdown(
        lint_digest={},
        test_digest={},
        audit_digest={
            "alignment_score": 0.9,
            "severity_counts": {"unknown": 4, "warning": 3},
            "outliers_found": 4,
            "parsed_outlier_items": 4,
            "drift_increased": True,
            "new_findings_count": 3,
            "new_findings": [
                {"context": "docs", "message": "Broken file reference", "fingerprint": "docs::broken"}
            ],
            "top_findings": [
                {"file": f"src/file-{idx}.rs", "rule": "broken_doc_reference", "message": "missing target"}
                for idx in range(6)
            ],
        },
        autofixability={"overall": "human_needed", "autofix_enabled": False, "autofix_attempted": False},
        run_url="https://github.com/Extra-Chill/homeboy/actions/runs/1",
        tooling={},
        job_links={},
        results={"audit": "fail"},
    )
    assert_contains(full_digest, "**TL;DR:** :x: audit (3 new)", "failure digest TL;DR")
    assert_contains(full_digest, ":x: **3 new finding(s) on this PR:**", "failure digest delta promotion")
    assert_contains(full_digest, "<details><summary>Audit findings (6)</summary>", "deduped details block")
    assert_not_contains(full_digest, "Top actionable findings", "failure digest duplicated top list")
    assert_not_contains(full_digest, "unknown:", "failure digest unknown severity")

    test_digest = render_markdown(
        lint_digest={},
        test_digest={
            "failed_tests_count": 0,
            "top_failed_tests": [],
            "raw_excerpt": ["composer install failed", "exit status 2"],
            "status": "failed",
            "exit_code": 1,
            "failure_message": "runner setup failed",
        },
        audit_digest={},
        autofixability={"overall": "human_needed", "autofix_enabled": False, "autofix_attempted": False},
        run_url="https://github.com/Extra-Chill/homeboy/actions/runs/1",
        tooling={},
        job_links={},
        results={"test": "fail"},
    )
    assert_contains(test_digest, "Test command failed, but structured output reported **0 failed test cases**.", "digest zero-failed test command explanation")
    assert_contains(test_digest, "runner/tooling failure", "digest test failure classification")
    assert_contains(test_digest, "<details><summary>Raw test failure excerpt</summary>", "digest raw log excerpt")
    assert_not_contains(test_digest, "Failed tests: **0**", "digest misleading zero failed tests line")

    print("comment quality rendering checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
