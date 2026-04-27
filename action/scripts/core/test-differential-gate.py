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
            {"audit": "pass"},
            run_gate({"audit": "fail"}, current, base),
            "audit failure passes when outliers do not increase",
        )

        write_json(current / "audit.json", {"success": False, "data": {"summary": {"outliers_found": 6}}})
        assert_equal(
            {"audit": "fail"},
            run_gate({"audit": "fail"}, current, base),
            "audit failure remains when outliers increase",
        )

        write_json(base / "test.json", {"success": False, "data": {"test_counts": {"failed": 2, "errors": 1}}})
        write_json(current / "test.json", {"success": False, "data": {"test_counts": {"failed": 2, "errors": 1}}})
        assert_equal(
            {"test": "pass"},
            run_gate({"test": "fail"}, current, base),
            "test failure passes when failures do not increase",
        )

        write_json(current / "test.json", {"success": False, "data": {"test_counts": {"failed": 3, "errors": 1}}})
        assert_equal(
            {"test": "fail"},
            run_gate({"test": "fail"}, current, base),
            "test failure remains when failures increase",
        )

        assert_equal(
            {"lint": "fail", "audit": "pass"},
            run_gate({"lint": "fail", "audit": "pass"}, current, base),
            "non-audit-test results are left untouched",
        )

        (base / "audit.json").unlink()
        (current / "audit.json").unlink()
        assert_equal(
            {"audit": "fail"},
            run_gate({"audit": "fail"}, current, base),
            "missing metric files preserve failures",
        )

    print("All differential gate checks passed.")


if __name__ == "__main__":
    main()
