#!/usr/bin/env bash

# Operations result files must be unique per action invocation.
#
# Regression guard for the sibling of #483. That issue fixed colliding artifact
# NAMES; the operations result JSON had the same defect and kept it:
#
#   run-operations.sh sets CMD_INDEX=0 at the top of every invocation and writes
#   "${HOMEBOY_OUTPUT_DIR}/operations-${CMD_INDEX}.json", while
#   HOMEBOY_OUTPUT_DIR is exported once for the whole job.
#
# So a job that invokes this action twice had its second invocation overwrite
# the first's operations-1.json. Nothing failed and nothing warned: the file
# still existed and still parsed, it just described a different command.
#
# Observed downstream: a deploy workflow ran `deploy --outdated` and then
# `deploy --check` in one job. The check invocation clobbered the deploy
# results, so a reporting step that counted `status == "deployed"` entries
# found zero and stayed silent after a deploy that had genuinely shipped two
# components.
#
# Consumers glob `operations-*.json`, so the stem must keep that prefix.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fail=0
pass() { echo "PASS: $1"; }
check_differs() {
  if [ "$2" != "$3" ]; then pass "$1"; else echo "FAIL: $1 (both '$2')"; fail=1; fi
}
check_contains() {
  case "$2" in
    *"$3"*) pass "$1" ;;
    *) echo "FAIL: $1 ('$2' does not contain '$3')"; fail=1 ;;
  esac
}

# Mirror of the stem construction in action.yml's "Resolve artifact names".
stem() {
  local artifact_suffix="${1}-${2}-${3}-${4}-${5}-${6}"
  artifact_suffix="$(printf '%s' "${artifact_suffix}" | tr -cs 'A-Za-z0-9._-' '-')"
  artifact_suffix="${artifact_suffix#-}"
  artifact_suffix="${artifact_suffix%-}"
  [ -z "${artifact_suffix}" ] && artifact_suffix="${4:-homeboy}"
  local operations_stem_suffix
  operations_stem_suffix="$(printf '%s' "${artifact_suffix}" | cut -c1-64)"
  operations_stem_suffix="${operations_stem_suffix%-}"
  printf 'operations-%s' "${operations_stem_suffix}"
}

# The exact downstream shape: deploy then verify, same job and runner.
deploy_stem="$(stem extrachill-network '' 'deploy extrachill-site --outdated' deploy __run Linux)"
verify_stem="$(stem extrachill-network '' 'deploy extrachill-site --check' deploy __run_2 Linux)"

check_differs "deploy and verify invocations write distinct stems" "$deploy_stem" "$verify_stem"

# Two invocations of the SAME command list must still differ. The instance id
# is what guarantees that, not the command text.
same_a="$(stem extrachill-network '' 'deploy extrachill-site --outdated' deploy __run Linux)"
same_b="$(stem extrachill-network '' 'deploy extrachill-site --outdated' deploy __run_3 Linux)"
check_differs "identical command lists in one job still differ" "$same_a" "$same_b"

# Consumers glob operations-*.json; the prefix is load-bearing.
check_contains "stem keeps the operations- prefix" "$deploy_stem" "operations-"

# Filenames have length limits, and the suffix embeds component, commands, job,
# instance and OS. The stem plus an index and extension must stay well clear.
long_stem="$(stem "$(printf 'c%.0s' {1..200})" "$(printf 'x%.0s' {1..200})" "$(printf 'y%.0s' {1..200})" job __run Linux)"
if [ "${#long_stem}" -le 96 ]; then
  pass "stem stays a legal filename length (${#long_stem} chars)"
else
  echo "FAIL: stem grew to ${#long_stem} chars"
  fail=1
fi

# The script must consume the stem rather than hardcoding "operations".
if grep -q 'OUTPUT_STEM="${OPERATIONS_OUTPUT_STEM:-operations}-${CMD_INDEX}"' \
  "${ROOT_DIR}/scripts/operations/run-operations.sh"; then
  pass "run-operations.sh derives the stem from OPERATIONS_OUTPUT_STEM"
else
  echo "FAIL: run-operations.sh no longer reads OPERATIONS_OUTPUT_STEM"
  fail=1
fi

# The action must actually pass it through, or the fallback silently restores
# the collision this guard exists to prevent.
if grep -q 'OPERATIONS_OUTPUT_STEM: ' "${ROOT_DIR}/action.yml"; then
  pass "action.yml passes OPERATIONS_OUTPUT_STEM to the operations step"
else
  echo "FAIL: action.yml does not export OPERATIONS_OUTPUT_STEM"
  fail=1
fi

if grep -q 'operations-stem=operations-' "${ROOT_DIR}/action.yml"; then
  pass "action.yml publishes an operations-stem output"
else
  echo "FAIL: action.yml does not publish operations-stem"
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "operations output uniqueness checks FAILED"
  exit 1
fi
echo "All operations output uniqueness checks passed."
