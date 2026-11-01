#!/usr/bin/env python3
"""Replay scoped Rust candidate identities against a fresh base inventory.

Never reuse candidate workspace fingerprints: a shard must be bound to the base
checkout. Enumeration compiles/lists tests but does not execute the full suite.
"""
import importlib.util
import json
import os
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location(
    "gate", Path(__file__).with_name("apply-differential-gate.py")
)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


def main():
    manifest = gate.read_json(os.environ["CANDIDATE_MANIFEST"]) or {}
    command = os.environ["COMMAND"]
    directory = str(Path(os.environ["CANDIDATE_MANIFEST"]).parent / "homeboy-ci-results")
    # Replay only the candidate's failures. The candidate inventory sidecar
    # lists every enumerated test, not just the executed scope (homeboy#16257:
    # 76 executed, 14,696 inventoried), and the gate verdict is
    # candidate_failed - base_failed, so base outcomes for passing tests are
    # never consulted.
    state, _inventory, identities = gate.test_outcomes(command, directory)
    inventory = gate.read_json(os.path.join(directory, f"{gate.output_stem(command)}.test-inventory.json")) or {}
    runner = inventory.get("runner")
    if sys.argv[1] == "plan":
        # Unsupported runners and absent evidence retain the existing baseline.
        scoped = (gate.quality_base_command(command) == "test"
                  and manifest.get("scope") == "changed"
                  and state == "complete" and bool(identities)
                  and runner in {"cargo", "nextest"})
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"runner={runner if scoped else ''}\n")
        return
    if state != "complete" or not identities:
        raise ValueError("Candidate selection is missing or invalid")
    base = gate.read_json(os.environ["BASELINE_INVENTORY_FILE"])
    if not base or base.get("schema") != "homeboy/test-inventory/v1" or base.get("runner") != runner:
        raise ValueError("Base inventory is missing or has a different runner")
    known = {test["id"] for test in base["tests"]}
    selected = sorted(identities & known)
    # New candidate-only tests remain absent from the base evidence, so the
    # differential gate still attributes their failures to the candidate.
    if not selected:
        raise ValueError("No candidate identities exist at base; refusing an empty shard or full-suite fallback")
    shard = {key: base[key] for key in (
        "runner", "runner_fingerprint", "workspace_fingerprint", "inventory_fingerprint"
    )}
    shard.update(schema="homeboy/test-shard-manifest/v1", id="baseline", tests=selected)
    path = Path(os.environ["BASELINE_SELECTION_FILE"]).resolve()
    path.write_text(json.dumps(shard, sort_keys=True) + "\n")
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"selection-file={path}\n")
    print(f"Baseline selected {len(selected)} candidate failure identities ({len(identities - known)} candidate-only)")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError) as error:
        sys.exit(f"::error::Cannot select baseline tests: {error}")
