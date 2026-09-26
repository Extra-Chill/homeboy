#!/usr/bin/env python3

from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "core" / "apply-differential-gate.py"


def write_json(path: Path, payload: dict) -> None:
    path.write_text(json.dumps(payload), encoding="utf-8")


def run_gate(results: dict, current: Path, base: Path) -> dict:
    completed = subprocess.run(
        ["python3", str(SCRIPT), json.dumps(results), str(current), str(base)],
        check=True,
        text=True,
        capture_output=True,
    )
    return json.loads(completed.stdout)


def assert_equal(expected, actual, label: str) -> None:
    if expected != actual:
        raise AssertionError(f"{label}\nexpected: {expected!r}\nactual:   {actual!r}")
    print(f"PASS: {label}")


def main() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        current = root / "current"
        base = root / "base"
        current.mkdir()
        base.mkdir()

        write_json(base / "audit.json", {"success": False, "data": {"summary": {"outliers_found": 5}}})
        write_json(current / "audit.json", {"success": False, "data": {"summary": {"outliers_found": 5}}})
        assert_equal(
            {"audit": "baseline_red"},
            run_gate({"audit": "fail"}, current, base),
            "audit failure that reproduces unchanged on the baseline is baseline_red",
        )

        write_json(current / "audit.json", {"success": False, "data": {"summary": {"outliers_found": 6}}})
        assert_equal(
            {"audit": "fail"},
            run_gate({"audit": "fail"}, current, base),
            "audit failure remains when outliers increase",
        )

        write_json(
            current / "audit.json",
            {
                "success": False,
                "data": {
                    "summary": {"outliers_found": 6},
                    "changed_since": {"introduced_findings": 0, "contextual_findings": 6},
                },
            },
        )
        assert_equal(
            {"audit": "pass"},
            run_gate({"audit": "fail"}, current, base),
            "changed-scope audit failure passes when no findings were introduced",
        )

        write_json(base / "test.json", {"success": False, "data": {"test_counts": {"failed": 2, "errors": 1}}})
        write_json(current / "test.json", {"success": False, "data": {"test_counts": {"failed": 2, "errors": 1}}})
        assert_equal(
            {"test": "no_comparable_evidence"},
            run_gate({"test": "fail"}, current, base),
            "aggregate test counts without outcome sidecars fail closed",
        )

        write_json(current / "test.json", {"success": False, "data": {"test_counts": {"failed": 3, "errors": 1}}})
        assert_equal(
            {"test": "no_comparable_evidence"},
            run_gate({"test": "fail"}, current, base),
            "aggregate test regressions require identity evidence",
        )

        write_json(
            current / "test.json",
            {
                "success": False,
                "data": {
                    "test_counts": {"failed": 3, "errors": 1},
                    "changed_since": {"introduced_failures": 0},
                },
            },
        )
        assert_equal(
            {"test": "no_comparable_evidence"},
            run_gate({"test": "fail"}, current, base),
            "changed-scope counts do not replace per-test outcomes",
        )

        assert_equal(
            {"lint": "inconclusive", "audit": "pass"},
            run_gate({"lint": "fail", "audit": "pass"}, current, base),
            "lint failure without comparable metrics is inconclusive",
        )

        (base / "audit.json").unlink()
        (current / "audit.json").unlink()
        assert_equal(
            {"audit": "inconclusive"},
            run_gate({"audit": "fail"}, current, base),
            "missing metric files are inconclusive",
        )

        # The candidate's own lint already compared the changed files against
        # the merge base (`baseline_provenance.compared`, `resolution: git_base`).
        # When the action's separate baseline run measures nothing (it linted
        # zero files), that compared evidence decides: new findings still fail
        # instead of degrading to warn-only `inconclusive`.
        # See Extra-Chill/homeboy-action#509.
        lint_dir = root / "lint"
        lint_current = lint_dir / "current"
        lint_base = lint_dir / "base"
        lint_current.mkdir(parents=True)
        lint_base.mkdir(parents=True)
        compared = {
            "base_ref": "e54332ba",
            "compared": True,
            "resolution": "git_base",
            "scope": "changed",
            "files": ["includes/a.php"],
        }
        write_json(
            lint_current / "review-lint.json",
            {
                "success": False,
                "status": "failed",
                "data": {
                    "status": "failed",
                    "findings": [{"id": "alignment"}],
                    "baseline_comparison": {"drift_increased": True, "delta": 1, "new_items": [{"fingerprint": "e28e568c"}]},
                    "baseline_provenance": compared,
                },
            },
        )
        write_json(
            lint_base / "review-lint.json",
            {"success": True, "status": "succeeded", "data": {"status": "passed", "hints": ["Lint ran no scopes: 8 changed file(s) were considered"]}},
        )
        write_json(lint_base / "baseline-status.json", {"review lint": {"status": "pass", "exit_code": 0, "structured_output": True}})
        assert_equal(
            {"review lint": "fail"},
            run_gate({"review lint": "fail"}, lint_current, lint_base),
            "new findings proven by the candidate's own compared baseline still fail when the external baseline measured nothing",
        )

        write_json(
            lint_current / "review-lint.json",
            {
                "success": False,
                "status": "failed",
                "data": {
                    "status": "failed",
                    "findings": [{"id": "pre-existing"}],
                    "baseline_comparison": {"drift_increased": False, "delta": 0, "new_items": []},
                    "baseline_provenance": compared,
                },
            },
        )
        assert_equal(
            {"review lint": "baseline_red"},
            run_gate({"review lint": "fail"}, lint_current, lint_base),
            "findings the candidate's compared baseline shows as pre-existing are baseline_red",
        )

        write_json(
            lint_current / "review-lint.json",
            {
                "success": False,
                "status": "failed",
                "data": {
                    "status": "failed",
                    "findings": [{"id": "alignment"}],
                    "baseline_comparison": {"drift_increased": True, "delta": 1, "new_items": [{"fingerprint": "e28e568c"}]},
                    "baseline_provenance": {**compared, "compared": False},
                },
            },
        )
        assert_equal(
            {"review lint": "inconclusive"},
            run_gate({"review lint": "fail"}, lint_current, lint_base),
            "an uncompared candidate baseline does not decide the verdict",
        )

    print("All differential gate checks passed.")


if __name__ == "__main__":
    main()
