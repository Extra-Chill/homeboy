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
assert_equals '{"review test":"no_comparable_evidence"}' "${result}" "missing per-test evidence fails closed"

printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":false}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"no_comparable_evidence"}' "${result}" "unstructured baseline does not bypass required per-test evidence"

printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${base_dir}/review-test.json"
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"no_comparable_evidence"}' "${result}" "aggregate counts cannot replace per-test evidence"

# Sharded workflow commands retain their original spelling. A bare `test`
# command therefore reads `test.json`, not the review-command filename.
printf '{"success":false,"data":{"test_counts":{"failed":2,"errors":0}}}\n' > "${current_dir}/test.json"
printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${base_dir}/test.json"
printf '{"test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"test":"no_comparable_evidence"}' "${result}" "bare test requires comparable per-test evidence"

printf '{"success":false,"data":{"test_counts":{"failed":2,"errors":0}}}\n' > "${current_dir}/review-test.json"
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"no_comparable_evidence"}' "${result}" "aggregate candidate regression is not attributed without identities"

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
assert_equals '{"review test":"no_comparable_evidence"}' "${result}" "both failed phases without outcomes never become no_measurement"

# The discriminator. Same unmeasurable baseline, but here the candidate DID
# measure -- so "this failure reproduces on main" is a claim the evidence can
# support, and the verdict must stay `baseline_red`. If the new branch were
# keyed on the baseline alone it would swallow this case too.
printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${current_dir}/review-test.json"
rm -f "${base_dir}/review-test.json"
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":false}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"no_comparable_evidence"}' "${result}" "a measured aggregate candidate still requires baseline outcomes"

# The important negative. A killed run reports FEWER failures than a healthy
# baseline (0 here, versus 1), so a naive count comparison reads the timeout as
# an improvement and marks it pass. A green gate for a suite that never
# finished is worse than the false red this change removes.
printf '%s\n' "${timed_out_payload}" > "${current_dir}/review-test.json"
printf '{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}\n' > "${base_dir}/review-test.json"
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"timeout"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"no_comparable_evidence"}' "${result}" "an incomplete run has no comparable per-test evidence"

# Against a healthy baseline a timeout is actionable and must keep blocking.
# `inconclusive` would only warn, which is how a red gate becomes background
# noise.
printf '%s\n' "${timed_out_payload}" > "${current_dir}/review-test.json"
rm -f "${base_dir}/review-test.json"
printf '{}\n' > "${base_dir}/baseline-status.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"timeout"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"no_comparable_evidence"}' "${result}" "a timeout against a healthy baseline has no comparable per-test evidence"

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

outcome_case() {
  local label="$1" candidate="$2" baseline="$3" expected="$4" mode="${5:-complete}"
  rm -rf "${current_dir}" "${base_dir}"
  mkdir -p "${current_dir}" "${base_dir}"
  printf '%s\n' "${base_failed_structured}" > "${base_dir}/baseline-status.json"
  for phase in current base; do
    values="${candidate}"; [ "${phase}" = base ] && values="${baseline}"
    if [ "${mode}" != missing ]; then
      printf '%s' "${values}" | jq -Rc --arg command 'review test' --arg fingerprint fixture 'split(",") | map(select(length > 0) | capture("(?<id>[^:]+):(?<outcome>.*)")) | {schema:"homeboy/test-outcomes/v1",command:$command,inventory_fingerprint:$fingerprint,failed_test_ids:[.[] | select(.outcome == "failed") | .id]}' > "${tmp_dir}/${phase}-outcomes.json"
      printf '%s' "${values}" | jq -Rc --arg command 'review test' --arg fingerprint fixture 'split(",") | map(select(length > 0) | capture("(?<id>[^:]+):")) | {schema:"homeboy/test-inventory/v1",command:$command,inventory_fingerprint:$fingerprint,tests:.}' > "${tmp_dir}/${phase}-inventory.json"
      target="${current_dir}"; [ "${phase}" = base ] && target="${base_dir}"
      cp "${tmp_dir}/${phase}-outcomes.json" "${target}/review-test.test-outcomes.json"
      cp "${tmp_dir}/${phase}-inventory.json" "${target}/review-test.test-inventory.json"
    fi
  done
  actual="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
  assert_equals "${expected}" "${actual}" "${label}"
}

counts() { printf '{"success":false,"data":{"test_counts":{"failed":%s,"errors":0}}}' "$1"; }

base_failed_structured='{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}'

# --- Retry evidence and named introduced identities (Extra-Chill/homeboy#14984) --
#
# A test that passes on retry (recorded in the {stem}.test-retry.json
# sidecar written by retry-introduced-test-failures.sh) is flaky and must
# not block. A test that fails every retry attempt stays introduced. Either
# way the identities involved are named in the annotation, not just counted.

compute_inventory_fingerprint() {
  python3 - "$@" <<'PY'
import json
import sys
from hashlib import sha256

command = sys.argv[1]
ids = sys.argv[2:]
canonical = {
    "command": command,
    "execution_fingerprint": "c" * 64,
    "runner": "nextest",
    "runner_fingerprint": "a" * 64,
    "schema": "homeboy/test-inventory/v1",
    "tests": [{"id": identity} for identity in sorted(ids)],
    "workspace_fingerprint": "b" * 64,
}
print(sha256(json.dumps(canonical, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest())
PY
}

write_outcome_fixture() {
  # write_outcome_fixture DIR COMMAND FAILED_ID... -- ALL_ID...
  local dir="$1" command="$2"
  shift 2
  local failed_ids=() all_ids=() collecting_failed=true
  for arg in "$@"; do
    if [ "${arg}" = "--" ]; then
      collecting_failed=false
      continue
    fi
    if [ "${collecting_failed}" = true ]; then
      failed_ids+=("${arg}")
    else
      all_ids+=("${arg}")
    fi
  done
  local fp stem
  if [ "${#all_ids[@]}" -gt 0 ]; then
    fp="$(compute_inventory_fingerprint "${command}" "${all_ids[@]}")"
  else
    fp="$(compute_inventory_fingerprint "${command}")"
  fi
  stem="$(printf '%s' "${command}" | sed -E 's/[^[:alnum:]._-]+/-/g; s/^-+//; s/-+$//')"
  jq -cn --arg command "${command}" --arg fp "${fp}" \
    --argjson failed "$(printf '%s\n' "${failed_ids[@]:-}" | jq -R 'select(length > 0)' | jq -sc .)" \
    '{schema:"homeboy/test-outcomes/v1",command:$command,runner:"nextest",runner_fingerprint:("a"*64),workspace_fingerprint:("b"*64),execution_fingerprint:("c"*64),inventory_fingerprint:$fp,failed_test_ids:$failed}' \
    > "${dir}/${stem}.test-outcomes.json"
  jq -cn --arg command "${command}" --arg fp "${fp}" \
    --argjson tests "$(printf '%s\n' "${all_ids[@]:-}" | jq -R 'select(length > 0) | {id:.}' | jq -sc .)" \
    '{schema:"homeboy/test-inventory/v1",command:$command,runner:"nextest",runner_fingerprint:("a"*64),workspace_fingerprint:("b"*64),execution_fingerprint:("c"*64),inventory_fingerprint:$fp,tests:$tests}' \
    > "${dir}/${stem}.test-inventory.json"
}

rm -rf "${current_dir}" "${base_dir}"
mkdir -p "${current_dir}" "${base_dir}"
write_outcome_fixture "${current_dir}" 'review test' id_a id_b -- id_a id_b id_c
write_outcome_fixture "${base_dir}" 'review test' id_c -- id_a id_c
printf '{"review test":{"status":"fail","exit_code":1,"command":"homeboy review test sample --path .","structured_output":true}}\n' > "${base_dir}/baseline-status.json"

result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"fail"}' "${result}" "introduced test failures without retry evidence remain blocking"

message="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}" 2>&1 1>/dev/null)"
case "${message}" in
  *"id_a"*"id_b"*) printf 'PASS: the rejection names the introduced test identities\n' ;;
  *) printf 'FAIL: rejection message did not name introduced identities\n%s\n' "${message}"; exit 1 ;;
esac


# One identity passed on retry (flaky, excluded); one never did (stays introduced).
jq -cn '{schema:"homeboy/test-retry/v1",command:"review test",runner:"nextest",attempted:["id_a","id_b"],flaky:["id_a"],still_failing:["id_b"],max_retries:2}' \
  > "${current_dir}/review-test.test-retry.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"fail"}' "${result}" "a still-failing identity keeps blocking even when a sibling is flaky"

message="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}" 2>&1 1>/dev/null)"
case "${message}" in
  *"flaky"*"id_a"*) printf 'PASS: the flaky identity is named in a warning\n' ;;
  *) printf 'FAIL: flaky exclusion was not announced with the identity name\n%s\n' "${message}"; exit 1 ;;
esac
case "${message}" in
  *"id_b"*) printf 'PASS: the still-blocking identity is still named in the error\n' ;;
  *) printf 'FAIL: still-failing identity was not named after partial retry\n%s\n' "${message}"; exit 1 ;;
esac

# Every introduced identity passed on retry: nothing left to block, and the
# pre-existing baseline_red machinery takes over exactly as it does for a
# candidate that never had a retry sidecar at all.
jq -cn '{schema:"homeboy/test-retry/v1",command:"review test",runner:"nextest",attempted:["id_a","id_b"],flaky:["id_a","id_b"],still_failing:[],max_retries:2}' \
  > "${current_dir}/review-test.test-retry.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"baseline_red"}' "${result}" "a fully retried-flaky introduced set is not blocking"

# A retry sidecar naming a different command must be ignored, not trusted --
# it must never manufacture a pass for identities it was not evidence for.
jq -cn '{schema:"homeboy/test-retry/v1",command:"test",runner:"nextest",attempted:["id_a","id_b"],flaky:["id_a","id_b"],still_failing:[],max_retries:2}' \
  > "${current_dir}/review-test.test-retry.json"
result="$(python3 "${APPLY_GATE}" '{"review test":"fail"}' "${current_dir}" "${base_dir}")"
assert_equals '{"review test":"fail"}' "${result}" "a retry sidecar with a mismatched command identity is ignored"

# --- --emit-introduced: the retry orchestrator's own contract ---------------
# retry-introduced-test-failures.sh calls this before it has a live
# workspace, so it knows what to retry and which runner produced it.
rm -f "${current_dir}/review-test.test-retry.json"
emitted="$(python3 "${APPLY_GATE}" --emit-introduced 'review test' "${current_dir}" "${base_dir}")"
assert_equals '{"introduced":["id_a","id_b"],"runner":"nextest"}' "${emitted}" "--emit-introduced reports the candidate-only identities and runner"

rm -rf "${current_dir}" "${base_dir}"
mkdir -p "${current_dir}" "${base_dir}"
emitted="$(python3 "${APPLY_GATE}" --emit-introduced 'review test' "${current_dir}" "${base_dir}")"
assert_equals '{"introduced":[],"runner":null}' "${emitted}" "--emit-introduced reports nothing without comparable evidence"

# --- A valid empty inventory is complete evidence (Extra-Chill/homeboy#15022) --
# A changed-scope run that correctly selects zero tests writes an empty
# inventory and no failures; it must read as complete, not invalid.
rm -rf "${current_dir}" "${base_dir}"
mkdir -p "${current_dir}" "${base_dir}"
write_outcome_fixture "${current_dir}" 'review test' --
write_outcome_fixture "${base_dir}" 'review test' --
empty_evidence="$(python3 - "${SCRIPT_DIR}/apply-differential-gate.py" "${current_dir}" <<'PY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("gate", sys.argv[1])
gate = importlib.util.module_from_spec(spec); spec.loader.exec_module(gate)
evidence, inventory, failed = gate.test_outcomes("review test", sys.argv[2])
print(f"{evidence}|{sorted(inventory or [])}|{sorted(failed or [])}")
PY
)"
assert_equals 'complete|[]|[]' "${empty_evidence}" "an empty but well-formed inventory is complete evidence with no failures"

printf 'All differential gate checks passed.\n'
