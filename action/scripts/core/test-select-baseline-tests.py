#!/usr/bin/env python3
"""Scoped baseline fixture: fresh base binding, selection, and gate verdicts."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent


def write(path, value):
    path.write_text(json.dumps(value))


def evidence(directory, ids, failed):
    inventory = dict(schema="homeboy/test-inventory/v1", command="review test",
                     runner="nextest", runner_fingerprint="a" * 64,
                     workspace_fingerprint="b" * 64, execution_fingerprint="c" * 64,
                     tests=[{"id": identity} for identity in sorted(ids)])
    inventory["inventory_fingerprint"] = hashlib.sha256(
        json.dumps(inventory, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    write(directory / "review-test.test-inventory.json", inventory)
    outcomes = {key: value for key, value in inventory.items() if key != "tests"}
    outcomes.update(schema="homeboy/test-outcomes/v1", failed_test_ids=failed)
    write(directory / "review-test.test-outcomes.json", outcomes)


with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    candidate = root / "candidate"
    candidate.mkdir()
    results = candidate / "homeboy-ci-results"
    results.mkdir()
    manifest = candidate / "manifest.json"
    write(manifest, {"scope": "changed"})
    evidence(results, ["selected-pass", "selected-fail", "new-test"], ["selected-fail"])
    base_inventory = root / "base.json"
    write(base_inventory, dict(schema="homeboy/test-inventory/v1", runner="nextest",
                              runner_fingerprint="d" * 64, workspace_fingerprint="e" * 64,
                              inventory_fingerprint="f" * 64,
                              tests=[{"id": identity} for identity in ["selected-pass", "selected-fail", "unrelated"]]))
    output = root / "output"
    selection = root / "selection.json"
    env = dict(os.environ, COMMAND="review test", CANDIDATE_MANIFEST=str(manifest),
               BASELINE_INVENTORY_FILE=str(base_inventory), BASELINE_SELECTION_FILE=str(selection),
               GITHUB_OUTPUT=str(output))

    def run(mode, success=True):
        result = subprocess.run(["python3", str(ROOT / "select-baseline-tests.py"), mode], env=env,
                                capture_output=True, text=True)
        assert (result.returncode == 0) == success, result.stderr

    run("plan")
    assert output.read_text() == "runner=nextest\n"
    run("materialize")
    shard = json.loads(selection.read_text())
    # Only the candidate's failures replay; passing identities and the
    # unrelated inventory never run on the baseline.
    assert shard["tests"] == ["selected-fail"], shard["tests"]
    assert shard["workspace_fingerprint"] == "e" * 64
    assert shard["runner_fingerprint"] == "d" * 64
    assert shard["inventory_fingerprint"] == "f" * 64
    assert "selection-file=" in output.read_text()

    # The baseline's normal runner-owned sidecars cover only selected tests.
    base = root / "base-results"
    base.mkdir()
    evidence(base, shard["tests"], ["selected-fail"])
    write(results / "review-test.json", {"test_counts": {"failed": 1}})
    write(base / "review-test.json", {"test_counts": {"failed": 1}})

    def verdict():
        result = subprocess.run(["python3", str(ROOT / "apply-differential-gate.py"),
                                 '{"review test":"fail"}', str(results), str(base)],
                                check=True, capture_output=True, text=True)
        return json.loads(result.stdout)["review test"]

    assert verdict() == "baseline_red"
    evidence(results, ["selected-pass", "selected-fail", "new-test"], ["new-test"])
    assert verdict() == "fail", "candidate-only failures must not be pardoned"
    # A candidate failure that passes at base is introduced, not pardoned.
    evidence(results, ["selected-pass", "selected-fail"], ["selected-pass"])
    evidence(base, ["selected-pass"], [])
    assert verdict() == "fail", "a failure passing at base is introduced"
    evidence(base, ["selected-fail"], ["selected-fail"])
    write(manifest, {"scope": "full"})
    output.write_text("")
    run("plan")
    assert output.read_text() == "runner=\n", "full candidate must retain full baseline"
    write(manifest, {"scope": "changed"})
    evidence(results, ["new-test"], ["new-test"])
    run("materialize", success=False)
    (results / "review-test.test-inventory.json").unlink()
    output.write_text("")
    run("plan")
    assert output.read_text() == "runner=\n", "missing evidence retains safe fallback"

# Verify the workflow actually transports and consumes the selection.
import yaml
workflow = yaml.safe_load((ROOT.parents[1] / ".github/workflows/ci.yml").read_text())
steps = workflow["jobs"]["baseline"]["steps"]
phase = next(step for step in steps if step.get("id") == "phase")
assert phase["env"]["HOMEBOY_TEST_SHARD_MANIFEST"] == "${{ steps.baseline-selection.outputs.selection-file }}"
assert phase["with"]["scope"] == "full"
assert next(step for step in steps if step.get("name") == "Enumerate base Test inventory without running tests")["env"]["HOMEBOY_TEST_INVENTORY_ONLY"] == "1"
# Homeboy rejects inventory output outside the fixed workspace output
# ("test inventory requested an output path outside the fixed workspace
# output"), so every baseline selection file must live under the workspace.
for step in steps:
    for key in ("HOMEBOY_TEST_INVENTORY_FILE", "BASELINE_INVENTORY_FILE", "BASELINE_SELECTION_FILE"):
        value = (step.get("env") or {}).get(key)
        if value is not None:
            assert value.startswith("${{ github.workspace }}/"), f"{key} must be under github.workspace: {value}"
        # Homeboy accepts exactly <cargo root>/homeboy-test-inventory.json
        # (TEST_INVENTORY_PUBLIC_FILE); any other name is RequestedPathRejected.
        if key in ("HOMEBOY_TEST_INVENTORY_FILE", "BASELINE_INVENTORY_FILE") and value is not None:
            assert value == "${{ github.workspace }}/homeboy-test-inventory.json", f"{key} must use Homeboy's fixed inventory name: {value}"
print("PASS: scoped baseline selects only candidate identities, binds base provenance, and preserves differential verdicts")
