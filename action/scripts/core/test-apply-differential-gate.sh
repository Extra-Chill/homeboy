#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPLY_GATE="${SCRIPT_DIR}/apply-differential-gate.py"

assert_equals() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual:   %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-differential-gate.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

current_dir="${tmp_dir}/current"
base_dir="${tmp_dir}/base"
mkdir -p "${current_dir}" "${base_dir}"

printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${current_dir}/review-test.json"
printf '{}\n' > "${base_dir}/baseline-status.json"

result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"inconclusive"}' "${result}" "missing baseline counts become inconclusive"

printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":false}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"baseline_red"}' "${result}" "red baseline without structured counts does not fail candidate"

printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${base_dir}/review-test.json"
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"pass"}' "${result}" "matching review test baseline failure count is accepted"

printf '{"success":false,"data":{"test_counts":{"failed":2,"errors":0}}}\n' > "${current_dir}/review-test.json"
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"fail"}' "${result}" "candidate regression still fails"

printf '{"success":false,"data":{"lint_findings":[{},{}]}}\n' > "${current_dir}/review-lint.json"
printf '{"success":false,"data":{"lint_findings":[{},{}]}}\n' > "${base_dir}/review-lint.json"
printf '{"review lint":{"status":"fail","exit_code":1,"command":"homeboy review lint sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review lint":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review lint":"pass"}' "${result}" "lint baseline count is compared"

# --- Timeout classification (Extra-Chill/homeboy#10639) -----------------------
#
# Fixtures below are the shape Homeboy actually emits when a test command
# exhausts its budget, recorded from Extra-Chill/homeboy run 30376771886
# (job 90334340546): exit 124, a stderr timeout marker, and an all-zero counts
# summary because the child was killed before it wrote its results sidecar.
#
# These assert the classified *result* of that recorded timeout, never the
# command line that produced it.

timed_out_payload='{"success":false,"data":{"exit_code":124,"failure":{"category":"infrastructure","phase":"test"},"raw_output":{"stderr_tail":"Homeboy command timed out after 1500000ms; terminated child process group before returning failure evidence."},"test_counts":{"failed":0,"errors":0}}}'

# A timeout that also happens on the baseline is pre-existing, exactly like a
# baseline-red test failure. Before this change `timeout` skipped the gate
# entirely and stayed a hard red no matter what the baseline did.
printf '%s\n' "${timed_out_payload}" > "${current_dir}/review-test.json"
rm -f "${base_dir}/review-test.json"
printf '{"review test":{"status":"timeout","exit_code":124,"command":"homeboy review test sample --path .","structured_output":false}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"timeout"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"baseline_red"}' "${result}" "a timeout that also times out on the baseline is pre-existing, not a candidate failure"

# The important negative. A killed run reports FEWER failures than a healthy
# baseline (0 here, versus 1), so a naive count comparison reads the timeout as
# an improvement and marks it pass. A green gate for a suite that never
# finished is worse than the false red this change removes.
printf '%s\n' "${timed_out_payload}" > "${current_dir}/review-test.json"
printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${base_dir}/review-test.json"
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"timeout"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"timeout"}' "${result}" "an incomplete run is never promoted to pass by comparing its truncated counts"

# Against a healthy baseline a timeout is actionable and must keep blocking.
# `inconclusive` would only warn, which is how a red gate becomes background
# noise.
printf '%s\n' "${timed_out_payload}" > "${current_dir}/review-test.json"
rm -f "${base_dir}/review-test.json"
printf '{}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"timeout"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"timeout"}' "${result}" "a timeout against a healthy baseline stays blocking rather than degrading to inconclusive"

# A timeout must never be laundered into the vocabulary of test failures.
for status in fail inconclusive pass; do
  if [ "${result}" = "{\"review test\":\"${status}\"}" ]; then
    printf 'FAIL: timeout was reclassified as %s\n' "${status}"
    exit 1
  fi
done
printf 'PASS: timeout is not reported as fail, pass, or inconclusive\n'

# A timeout must read differently from a test failure in the PR comment, so a
# reviewer can tell them apart without opening the run.
#
# Deliberately not guarded by `if [ -f ... ]`: a guard that quietly skips when
# the path is wrong is how a check passes for weeks while asserting nothing.
# If sections.sh moves or stops sourcing, this must fail loudly.
ACTION_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SECTIONS="${ACTION_ROOT}/pr/comment/sections.sh"

if [ ! -f "${SECTIONS}" ]; then
  printf 'FAIL: expected PR comment renderer at %s\n' "${SECTIONS}"
  exit 1
fi

render_status() {
  GITHUB_ACTION_PATH="$(cd "${ACTION_ROOT}/.." && pwd)" \
    bash -c 'source "$1"; "$2" "$3"' _ "${SECTIONS}" "$1" "$2"
}

timeout_icon="$(render_status status_icon timeout)"
fail_icon="$(render_status status_icon fail)"
timeout_label="$(render_status status_label timeout)"
fail_label="$(render_status status_label fail)"

# Prove the renderer actually ran before comparing, so an empty-vs-empty
# comparison can never be mistaken for agreement.
[ -n "${timeout_icon}" ] || { printf 'FAIL: status_icon timeout produced no output\n'; exit 1; }
[ -n "${fail_icon}" ] || { printf 'FAIL: status_icon fail produced no output\n'; exit 1; }
[ -n "${timeout_label}" ] || { printf 'FAIL: status_label timeout produced no output\n'; exit 1; }
[ -n "${fail_label}" ] || { printf 'FAIL: status_label fail produced no output\n'; exit 1; }

assert_equals 'distinct' \
  "$([ "${timeout_icon}" != "${fail_icon}" ] && echo distinct || echo same)" \
  "timeout renders a different icon (${timeout_icon}) from a test failure (${fail_icon})"
assert_equals 'distinct' \
  "$([ "${timeout_label}" != "${fail_label}" ] && echo distinct || echo same)" \
  "timeout renders a different label (${timeout_label}) from a test failure (${fail_label})"

printf 'All differential gate checks passed.\n'
