#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/ci.yml"
PROBE="${ROOT_DIR}/scripts/core/check-pr-state.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

mkdir -p "${TMP_DIR}/bin"
cat > "${TMP_DIR}/bin/gh" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "${PR_STATE}"
SH
chmod +x "${TMP_DIR}/bin/gh"

assert_pr_state() {
  local state="$1" expected="$2" label="$3"
  : > "${TMP_DIR}/output"
  PR_STATE="${state}" PR_NUMBER=419 GITHUB_REPOSITORY=Extra-Chill/homeboy-action \
    GITHUB_OUTPUT="${TMP_DIR}/output" PATH="${TMP_DIR}/bin:${PATH}" bash "${PROBE}" >/dev/null
  if ! grep -Fxq "active=${expected}" "${TMP_DIR}/output"; then
    printf 'FAIL: %s did not report active=%s\n' "${label}" "${expected}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

# A closure can arrive while any candidate phase is completing; the finalizer
# must suppress its synthetic verdict for both closed and merged PRs.
assert_pr_state CLOSED false 'closed candidate suppresses finalization'
assert_pr_state MERGED false 'merged candidate suppresses finalization'
assert_pr_state OPEN true 'active candidate remains eligible for finalization'

python3 - "${WORKFLOW}" <<'PY'
import sys
import yaml

workflow = yaml.safe_load(open(sys.argv[1]))
problems = []
for job_name in ("reconcile", "reconcile-test-shards"):
    steps = workflow["jobs"][job_name]["steps"]
    probe = next((i for i, step in enumerate(steps) if step.get("name") == "Check final PR state"), None)
    if probe is None:
        problems.append(f"{job_name} does not re-check PR state before finalization")
        continue
    if steps[probe].get("id") != "final-pr-state":
        problems.append(f"{job_name} final PR state probe lacks its output id")
    if "always()" not in steps[probe].get("if", ""):
        problems.append(f"{job_name} final PR state probe does not run after cancelled dependencies")
    for step in steps[probe + 1:]:
        condition = step.get("if", "")
        if "steps.final-pr-state.outputs.active != 'false'" not in condition:
            name = step.get("name", step.get("uses", "unnamed step"))
            problems.append(f"{job_name} finalization step {name!r} can run after PR closure")

if problems:
    print("\n".join(problems))
    sys.exit(1)
PY

# `always()` is intentional for active candidates: cancelled dependencies must
# fail closed rather than silently becoming a passing required gate. Closed or
# merged PRs reach the same finalizer, then every remaining step is skipped.
grep -F "if: \${{ !inputs.contract-probe && always() && needs.plan.outputs.non-test-commands-enabled == 'true' }}" "${WORKFLOW}" >/dev/null \
  || { printf 'FAIL: active non-Test cancellation can bypass reconciliation\n'; exit 1; }
grep -F "if: \${{ !inputs.contract-probe && always() && needs.plan.outputs.test-shards-enabled == 'true' }}" "${WORKFLOW}" >/dev/null \
  || { printf 'FAIL: active sharded Test cancellation can bypass reconciliation\n'; exit 1; }
grep -F "steps.final-pr-state.outputs.active != 'false' && needs.candidate-test-plan.result != 'success'" "${WORKFLOW}" >/dev/null \
  || { printf 'FAIL: active Test inventory failure is not terminal\n'; exit 1; }
grep -F "steps.final-pr-state.outputs.active != 'false' && needs.candidate-test-plan.result == 'success' && needs.candidate-test-result.result != 'success'" "${WORKFLOW}" >/dev/null \
  || { printf 'FAIL: active Test aggregation failure is not terminal\n'; exit 1; }

printf 'PASS: reusable finalizers suppress inactive PRs and fail active cancellations or failures\n'
