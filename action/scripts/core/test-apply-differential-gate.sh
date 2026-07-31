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
assert_equals '{"review test":"baseline_red"}' "${result}" "a failure that reproduces unchanged on the baseline is baseline_red, not pass"

printf '{"success":false,"data":{"test_counts":{"failed":2,"errors":0}}}\n' > "${current_dir}/review-test.json"
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"fail"}' "${result}" "candidate regression still fails"

printf '{"success":false,"data":{"lint_findings":[{},{}]}}\n' > "${current_dir}/review-lint.json"
printf '{"success":false,"data":{"lint_findings":[{},{}]}}\n' > "${base_dir}/review-lint.json"
printf '{"review lint":{"status":"fail","exit_code":1,"command":"homeboy review lint sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review lint":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review lint":"baseline_red"}' "${result}" "lint baseline count is compared and an unchanged count is baseline_red"

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

# A timeout that also happens on the baseline must not fail the candidate.
# Before this was admitted to the gate, `timeout` skipped it entirely and stayed
# a hard red no matter what the baseline did.
#
# It is reported as `no_measurement` rather than `baseline_red`. Both are
# non-blocking, so the decision above is unchanged -- but `baseline_red` claims
# the failure is *pre-existing*, and that claim needs an observation on the
# candidate side to rest on. Here neither side wrote counts, so nothing is known
# and there is nothing to call pre-existing. See Extra-Chill/homeboy#10999.
printf '%s\n' "${timed_out_payload}" > "${current_dir}/review-test.json"
rm -f "${base_dir}/review-test.json"
printf '{"review test":{"status":"timeout","exit_code":124,"command":"homeboy review test sample --path .","structured_output":false}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"timeout"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"no_measurement"}' "${result}" "a double timeout reports no_measurement, not a pre-existing-failure diagnosis"

# The discriminator. Same unmeasurable baseline, but here the candidate DID
# measure -- so "this failure reproduces on main" is a claim the evidence can
# support, and the verdict must stay `baseline_red`. If the new branch were
# keyed on the baseline alone it would swallow this case too.
printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${current_dir}/review-test.json"
rm -f "${base_dir}/review-test.json"
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":false}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"baseline_red"}' "${result}" "a measured candidate against an unmeasurable baseline is still baseline_red"

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

# --- Equal-failure laundering + zero-count invariant -------------------------
# Extra-Chill/homeboy#10657 (pass synthesised from two failing runs) and
# Extra-Chill/homeboy#10685 (a gate must never pass without having measured
# something).
#
# Every case below feeds a recorded payload shape through the classifier and
# asserts the resulting *verdict*. None of them assert a command string: the
# post-merge audit gate passed for weeks while asserting only the command it
# ran, which is how it scanned zero files undetected.

# Reset both fixture dirs so no earlier case can leak a stale sidecar into a
# later one and make it assert against a payload it did not write.
gate_case() {
  local label="$1" results="$2" current_payload="$3" base_payload="$4" base_status="$5" expected="$6"

  rm -rf "${current_dir}" "${base_dir}"
  mkdir -p "${current_dir}" "${base_dir}"

  [ "${current_payload}" = "-" ] || printf '%s\n' "${current_payload}" > "${current_dir}/review-test.json"
  [ "${base_payload}" = "-" ] || printf '%s\n' "${base_payload}" > "${base_dir}/review-test.json"
  printf '%s\n' "${base_status}" > "${base_dir}/baseline-status.json"

  local actual
  actual="$(python3 "${APPLY_GATE}" "${results}" "${current_dir}" "${base_dir}")"

  # Prove the classifier actually produced a verdict before comparing, so an
  # empty-vs-empty comparison can never be mistaken for agreement.
  if [ -z "${actual}" ]; then
    printf 'FAIL: %s\nthe gate produced no output at all\n' "${label}"
    exit 1
  fi

  assert_equals "${expected}" "${actual}" "${label}"

  # The invariant this whole section exists to defend, asserted independently
  # of the equality check above: nothing here may render a green check.
  if [ "${expected}" != '{"review test":"pass"}' ] && [ "${actual}" = '{"review test":"pass"}' ]; then
    printf 'FAIL: %s was laundered into pass\n' "${label}"
    exit 1
  fi
}

counts() { printf '{"success":false,"data":{"test_counts":{"failed":%s,"errors":0}}}' "$1"; }

base_failed_structured='{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}'

# 1/1 -- the defect. Both phases completed and reported one real failure. This
# is Extra-Chill/homeboy run 30380078587, which rendered a fully green check
# over a test that was red on main for twelve hours across 23 consecutive runs.
gate_case "1/1: an equal, non-zero failure count is baseline_red rather than a green check" \
  '{"review test":"fail"}' "$(counts 1)" "$(counts 1)" "${base_failed_structured}" \
  '{"review test":"baseline_red"}'

# 1/0 -- a real regression must still block. Proves the split did not make the
# gate permissive.
gate_case "1/0: a candidate that adds a failure against a clean baseline still fails" \
  '{"review test":"fail"}' "$(counts 1)" "$(counts 0)" "${base_failed_structured}" \
  '{"review test":"fail"}'

# 1/3 -- a genuine partial improvement is still accepted, so the split did not
# make the gate over-strict either. This is the branch that, across the 81-run
# window in #10657, fired exactly zero times.
gate_case "1/3: a genuine reduction in failures is still accepted" \
  '{"review test":"fail"}' "$(counts 1)" "$(counts 3)" "${base_failed_structured}" \
  '{"review test":"pass"}'

# 0/1 -- looks like "the PR fixed the last failure", but it cannot be. A run
# that genuinely reaches zero failures exits 0, is classified `pass` upstream in
# run-homeboy-commands.sh, and never enters the gate at all. Reaching this
# branch means the command failed while reporting nothing, so the count does
# not describe the failure and cannot license a pass.
gate_case "0/1: a failing command that reports zero failures is uncounted, not improved" \
  '{"review test":"fail"}' "$(counts 0)" "$(counts 1)" "${base_failed_structured}" \
  '{"review test":"inconclusive"}'

# 0/0 -- nothing was measured on either side. #10685: a comparison with no
# findings on either side is never a pass.
gate_case "0/0: a comparison in which neither side measured anything is never pass" \
  '{"review test":"fail"}' "$(counts 0)" "$(counts 0)" "${base_failed_structured}" \
  '{"review test":"inconclusive"}'

# A killed run, classified `fail` rather than `timeout`. run-homeboy-commands.sh
# only maps exit 124 to `timeout`; the SIGKILL/containment paths in
# run-with-liveness-timeout.sh exit 125, and an OOM kill surfaces as 137. Those
# land here as an ordinary `fail` carrying `failed: 0`, which is precisely the
# shape #305's timeout guard does not cover.
killed_as_fail='{"success":false,"data":{"exit_code":137,"failure":{"category":"infrastructure","phase":"test"},"raw_output":{"stderr_tail":"terminated child process group before returning failure evidence."},"test_counts":{"failed":0,"errors":0}}}'
gate_case "a killed run reported as fail (exit 137, failed:0) is never promoted to pass" \
  '{"review test":"fail"}' "${killed_as_fail}" "$(counts 1)" "${base_failed_structured}" \
  '{"review test":"inconclusive"}'

containment_kill='{"success":false,"data":{"exit_code":125,"test_counts":{"failed":0,"errors":0}}}'
gate_case "a containment-kill run (exit 125, failed:0) is never promoted to pass" \
  '{"review test":"fail"}' "${containment_kill}" "$(counts 4)" "${base_failed_structured}" \
  '{"review test":"inconclusive"}'

# Changed-scope gating must survive all of the above. Here zero is a real
# measurement -- "this change introduced nothing" -- not an uncounted failure,
# so it must still pass even though the repository is red and the raw counts
# are non-zero. If the zero guard were applied blindly this would regress.
scoped_clean='{"success":false,"data":{"test_counts":{"failed":3,"errors":1},"changed_since":{"introduced_failures":0}}}'
gate_case "changed-scope zero introduced failures still passes against a red baseline" \
  '{"review test":"fail"}' "${scoped_clean}" "$(counts 3)" "${base_failed_structured}" \
  '{"review test":"pass"}'

printf 'All differential gate checks passed.\n'
