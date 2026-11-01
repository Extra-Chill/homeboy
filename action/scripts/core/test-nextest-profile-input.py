#!/usr/bin/env python3
"""The reusable workflow exports the caller's nextest profile to every
unsharded Test phase. Passthrough `-- --profile` cannot be used there: the
Rust runner rejects passthrough args under changed-scope selection and shard
replay (Extra-Chill/homeboy#16264)."""
from pathlib import Path
import yaml

workflow = yaml.safe_load((Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml").read_text())
inputs = workflow[True]["workflow_call"]["inputs"]
assert inputs["nextest-profile"]["default"] == ""
expected = "${{ inputs.nextest-profile || 'default' }}"
for job in ("candidate", "baseline", "reconcile"):
    assert workflow["jobs"][job].get("env", {}).get("NEXTEST_PROFILE") == expected, job
# Shard archive build and replay keep their pinned profile at step level.
shard_steps = [step for job in ("candidate-test-plan", "candidate-test-shards")
               for step in workflow["jobs"][job]["steps"]
               if (step.get("env") or {}).get("NEXTEST_PROFILE") == "ci"]
assert shard_steps, "shard steps lost their pinned ci profile"
print("PASS: nextest-profile reaches candidate, baseline, and retry Test phases")
