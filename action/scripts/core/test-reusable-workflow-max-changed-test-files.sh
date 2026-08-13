#!/usr/bin/env bash

# The changed-scope test selection cap (HOMEBOY_MAX_CHANGED_TEST_FILES) must be
# reachable from the reusable workflow so consumers can bound how many test
# files Homeboy selects for a diff without forking the workflow. Homeboy core
# reads the variable from the process environment, so the workflow only has to
# forward the opt-in input to the phases that genuinely execute a changed-scope
# test command. The default must stay inert: an unset consumer sees zero
# behaviour change.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/ci.yml"

python3 - "${WORKFLOW}" <<'PY'
import sys
import yaml

workflow = yaml.safe_load(open(sys.argv[1]))
problems = []

on_key = next(k for k in workflow if isinstance(workflow[k], dict) and "workflow_call" in workflow[k])
inputs = workflow[on_key]["workflow_call"]["inputs"]

cap = inputs.get("max-changed-test-files")
if cap is None:
    problems.append("missing workflow_call input max-changed-test-files")
elif cap.get("type") != "string":
    problems.append("max-changed-test-files must be a string input")
elif cap.get("default") != "":
    problems.append("max-changed-test-files must default to '' (disabled, behaviour unchanged)")

def phase_step(job_name, step_name):
    for step in workflow["jobs"][job_name].get("steps", []):
        if step.get("name") == step_name:
            return step
    return None

expected = "${{ inputs.max-changed-test-files }}"
for job_name, step_name in [
    ("candidate", "Run candidate ${{ matrix.title }}"),
    ("baseline", "Run baseline ${{ matrix.title }}"),
]:
    step = phase_step(job_name, step_name)
    if step is None:
        problems.append(f"{job_name} has no {step_name!r} step")
        continue
    forwarded = (step.get("env") or {}).get("HOMEBOY_MAX_CHANGED_TEST_FILES")
    if forwarded != expected:
        problems.append(
            f"{job_name} must forward the input value {expected!r}, got {forwarded!r}"
        )

for job_name, step_name in [
    ("candidate-test-plan", "Configure candidate Test shards"),
    ("candidate-test-shards", "Run candidate Test shard ${{ matrix.shard_id }}"),
]:
    step = phase_step(job_name, step_name)
    if step is None:
        problems.append(f"{job_name} has no {step_name!r} step")
        continue
    if "HOMEBOY_MAX_CHANGED_TEST_FILES" in (step.get("env") or {}):
        problems.append(f"{job_name} must not set HOMEBOY_MAX_CHANGED_TEST_FILES on its {step_name!r} step")

if problems:
    print("\n".join(problems))
    sys.exit(1)
PY

# Belt-and-braces pin on the forwarding mechanism itself: the workflow must
# read the value from the input, never hardcode a number.
if [ "$(grep -Fc 'HOMEBOY_MAX_CHANGED_TEST_FILES: ${{ inputs.max-changed-test-files }}' "${WORKFLOW}")" -ne 2 ]; then
  printf 'FAIL: candidate and baseline phases must forward HOMEBOY_MAX_CHANGED_TEST_FILES from the input exactly twice\n'
  exit 1
fi
if [ "$(grep -cE 'HOMEBOY_MAX_CHANGED_TEST_FILES: [0-9]' "${WORKFLOW}")" -ne 0 ]; then
  printf 'FAIL: HOMEBOY_MAX_CHANGED_TEST_FILES must never be hardcoded in the workflow\n'
  exit 1
fi

printf 'PASS: reusable workflow exposes an inert max-changed-test-files cap on changed-scope phases\n'
