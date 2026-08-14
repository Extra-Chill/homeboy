#!/usr/bin/env python3

"""Validate the minimal caller contract for the published reusable workflow.

GitHub does not execute nested reusable workflows locally. This checks the
caller/callee semantics GitHub evaluates before scheduling any jobs, including
the permission rule that caused the #327/#328 startup failure.
"""

from pathlib import Path
import sys

import yaml


ROOT = Path(__file__).resolve().parents[2]
CALLEE_PATH = ROOT / ".github/workflows/ci.yml"
CALLER_PATH = ROOT / "fixtures/reusable-workflow-minimal-consumer.yml"
NEGATIVE_CALLEE_PATH = ROOT / "fixtures/reusable-workflow-unavailable-permission-callee.yml"
PROBE_PATH = ROOT / ".github/workflows/consumer-contract.yml"
README_PATH = ROOT / "README.md"
EXPECTED_JOBS = {
    "plan": "Plan",
    "binary": "Prepare candidate Homeboy binary",
    "candidate": "Candidate ${{ matrix.title }}",
    "reconcile": "${{ matrix.title }}",
}
NORMAL_JOBS = {
    "binary",
    "plan",
    "candidate",
    "baseline",
    "candidate-test-plan",
    "candidate-test-shards",
    "candidate-test-result",
    "reconcile-test-shards",
    "reconcile",
    "policy",
}
PERMISSION_LEVELS = {"none": 0, "read": 1, "write": 2}


def load_workflow(path):
    with path.open() as workflow:
        return yaml.safe_load(workflow)


def fail(message):
    print(f"FAIL: {message}")
    sys.exit(1)


callee = load_workflow(CALLEE_PATH)
caller = load_workflow(CALLER_PATH)
negative_callee = load_workflow(NEGATIVE_CALLEE_PATH)
probe = load_workflow(PROBE_PATH)

if "workflow_call" not in callee.get(True, callee.get("on", {})):
    fail("ci.yml is not callable with workflow_call")

if "fixtures/reusable-workflow-minimal-consumer.yml" not in README_PATH.read_text():
    fail("README does not document the canonical minimal reusable-workflow caller")

jobs = caller.get("jobs", {})
if set(jobs) != {"homeboy"}:
    fail("minimal caller must contain exactly the homeboy reusable-workflow job")

caller_job = jobs["homeboy"]
if caller_job.get("uses") != "Extra-Chill/homeboy-action/.github/workflows/ci.yml@v2":
    fail("minimal caller must use the published v2 reusable workflow")
if "runs-on" in caller_job:
    fail("reusable-workflow caller job must not declare runs-on")
if caller_job.get("secrets") != "inherit":
    fail("minimal caller must inherit caller secrets")

inputs = callee[True if True in callee else "on"]["workflow_call"].get("inputs", {})
if inputs.get("contract-probe", {}).get("type") != "boolean":
    fail("contract-probe must remain a boolean workflow_call input")
for input_name, value in caller_job.get("with", {}).items():
    if input_name not in inputs:
        fail(f"caller passes undocumented workflow_call input {input_name!r}")
    if inputs[input_name].get("type") != "string" or not isinstance(value, str):
        fail(f"caller input {input_name!r} is not compatible with its string workflow_call contract")

for input_name, definition in inputs.items():
    if definition.get("required") is True and input_name not in caller_job.get("with", {}):
        fail(f"minimal caller omits required workflow_call input {input_name!r}")

caller_permissions = caller.get("permissions", {})
callee_permissions = callee.get("permissions", {})


def assert_permission_contract(required_permissions, label, expected_to_pass):
    insufficient = []
    for scope, required_level in required_permissions.items():
        granted_level = caller_permissions.get(scope, "none")
        if required_level not in PERMISSION_LEVELS or granted_level not in PERMISSION_LEVELS:
            fail(f"unsupported permission level for {scope!r}")
        if PERMISSION_LEVELS[granted_level] < PERMISSION_LEVELS[required_level]:
            insufficient.append(f"{scope}: {granted_level} < {required_level}")

    if expected_to_pass and insufficient:
        fail(f"caller cannot start {label}: {', '.join(insufficient)}")
    if not expected_to_pass and not insufficient:
        fail(f"negative {label} unexpectedly permits the minimal caller")


assert_permission_contract(callee_permissions, "ci.yml", True)
assert_permission_contract(negative_callee.get("permissions", {}), "unavailable-permission fixture", False)

missing_jobs = set(EXPECTED_JOBS) - set(callee.get("jobs", {}))
if missing_jobs:
    fail(f"ci.yml no longer exposes required consumer jobs: {', '.join(sorted(missing_jobs))}")

for job_name, expected_name in EXPECTED_JOBS.items():
    if callee["jobs"][job_name].get("name") != expected_name:
        fail(f"ci.yml job {job_name!r} no longer exposes {expected_name!r} to callers")

probe_job = probe.get("jobs", {}).get("homeboy", {})
if probe_job.get("uses") != "./.github/workflows/ci.yml":
    fail("GitHub consumer probe does not invoke the promotion-candidate workflow")
if probe_job.get("with", {}).get("contract-probe") is not True:
    fail("GitHub consumer probe does not use bounded contract-probe mode")
if probe_job.get("secrets") != "inherit" or "runs-on" in probe_job:
    fail("GitHub consumer probe is not a valid reusable-workflow caller job")

contract_job = callee.get("jobs", {}).get("contract-probe", {})
if contract_job.get("name") != "Consumer contract" or contract_job.get("if") != "inputs.contract-probe":
    fail("ci.yml does not expose the bounded Consumer contract job")

for job_name in NORMAL_JOBS:
    condition = callee["jobs"].get(job_name, {}).get("if", "")
    if "inputs.contract-probe" not in condition:
        fail(f"normal job {job_name!r} can run during the bounded contract probe")

print("PASS: minimal documented caller satisfies reusable workflow permissions and inputs")
print("PASS: reusable workflow exposes Plan, candidate preparation, candidate, and verdict jobs")
print("PASS: unavailable permission fixture is rejected before job scheduling")
print("PASS: GitHub consumer probe invokes the promotion candidate in bounded mode")
