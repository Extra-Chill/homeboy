#!/usr/bin/env python3
"""Exercise Homeboy-owned per-test outcome/inventory reconciliation fixtures."""

from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).parent
GATE = ROOT / "apply-differential-gate.py"
FIXTURES = ROOT / "fixtures" / "differential-test-outcomes"
COMMAND = "review test"
STEM = "review-test"
BASELINE_STATUS = {COMMAND: {"status": "fail", "exit_code": 1, "structured_output": True}}


def gate(current: Path, baseline: Path) -> dict:
    completed = subprocess.run(
        ["python3", str(GATE), json.dumps({COMMAND: "fail"}), str(current), str(baseline)],
        check=True,
        text=True,
        capture_output=True,
    )
    return json.loads(completed.stdout)


def install_pair(directory: Path, prefix: str) -> None:
    shutil.copy(FIXTURES / f"{prefix}-outcomes.json", directory / f"{STEM}.test-outcomes.json")
    shutil.copy(FIXTURES / f"{prefix}-inventory.json", directory / f"{STEM}.test-inventory.json")


def main() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        current, baseline = root / "current", root / "baseline"
        current.mkdir()
        baseline.mkdir()
        (baseline / "baseline-status.json").write_text(json.dumps(BASELINE_STATUS), encoding="utf-8")

        install_pair(current, "13290-candidate")
        install_pair(baseline, "13290-baseline")
        assert gate(current, baseline) == {COMMAND: "fail"}, "#13290 nondeterministic identity must block"

        for path in current.iterdir():
            path.unlink()
        for path in baseline.iterdir():
            if path.name != "baseline-status.json":
                path.unlink()
        shutil.copy(FIXTURES / "1195-candidate-result.json", current / f"{STEM}.json")
        shutil.copy(FIXTURES / "1195-baseline-result.json", baseline / f"{STEM}.json")
        assert gate(current, baseline) == {COMMAND: "no_comparable_evidence"}, "#1195 must fail closed"

        install_pair(current, "13290-candidate")
        install_pair(baseline, "13290-baseline")
        payload = json.loads((current / f"{STEM}.test-outcomes.json").read_text(encoding="utf-8"))
        payload["outcomes"].append(payload["outcomes"][0])
        (current / f"{STEM}.test-outcomes.json").write_text(json.dumps(payload), encoding="utf-8")
        assert gate(current, baseline) == {COMMAND: "invalid_evidence"}, "duplicate identities must fail closed"

        install_pair(current, "13290-candidate")
        payload = json.loads((current / f"{STEM}.test-inventory.json").read_text(encoding="utf-8"))
        payload["inventory_fingerprint"] = "0" * 64
        (current / f"{STEM}.test-inventory.json").write_text(json.dumps(payload), encoding="utf-8")
        assert gate(current, baseline) == {COMMAND: "invalid_evidence"}, "arbitrary inventory fingerprints must fail closed"

        install_pair(current, "13290-candidate")
        payload = json.loads((current / f"{STEM}.test-outcomes.json").read_text(encoding="utf-8"))
        for outcome in payload["outcomes"]:
            outcome["outcome"] = "passed"
        (current / f"{STEM}.test-outcomes.json").write_text(json.dumps(payload), encoding="utf-8")
        assert gate(current, baseline) == {COMMAND: "invalid_evidence"}, "failed commands need a failed candidate identity"

    print("PASS: Test outcome/inventory reconciliation fixtures")


if __name__ == "__main__":
    main()
