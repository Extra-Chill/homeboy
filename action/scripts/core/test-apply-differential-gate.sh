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

printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${current_dir}/test.json"
printf '{}\n' > "${base_dir}/baseline-status.json"

result="$(python3 "${APPLY_GATE}" '{"test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"test":"inconclusive"}' "${result}" "missing baseline counts become inconclusive"

printf '{"test":{"status":"fail","exit_code":1,"command":"homeboy test sample --path .","structured_output":false}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"test":"baseline_red"}' "${result}" "red baseline without structured counts does not fail candidate"

printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${base_dir}/test.json"
printf '{"test":{"status":"fail","exit_code":1,"command":"homeboy test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"test":"pass"}' "${result}" "matching baseline failure count is accepted"

printf '{"success":false,"data":{"test_counts":{"failed":2,"errors":0}}}\n' > "${current_dir}/test.json"
result="$(python3 "${APPLY_GATE}" '{"test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"test":"fail"}' "${result}" "candidate regression still fails"

printf '{"success":false,"data":{"lint_findings":[{},{}]}}\n' > "${current_dir}/lint.json"
printf '{"success":false,"data":{"lint_findings":[{},{}]}}\n' > "${base_dir}/lint.json"
printf '{"lint":{"status":"fail","exit_code":1,"command":"homeboy lint sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"lint":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"lint":"pass"}' "${result}" "lint baseline count is compared"

printf 'All differential gate checks passed.\n'
