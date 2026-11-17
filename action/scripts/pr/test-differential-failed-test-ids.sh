#!/usr/bin/env bash
# Regression for Extra-Chill/homeboy#18363: a differential test verdict must
# name the failing tests that are new relative to the baseline (capped in the
# comment), or say exactly why it cannot, in both the PR comment section and
# the command's own review-test.json.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export GITHUB_ACTION_PATH="${ROOT}"
source "${ROOT}/scripts/core/lib.sh"
source "${ROOT}/scripts/pr/comment/sections.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
GATE="${ROOT}/scripts/core/apply-differential-gate.py"

export COMMANDS='review test' SCOPE_MODE=full TEST_SCOPE_EFFECTIVE=full
export COMP_ID=users GITHUB_WORKFLOW=Homeboy GITHUB_JOB=component-ci SECTION_TITLE=users

reset_dirs() {
  rm -rf "${TMP}/candidate" "${TMP}/base"
  mkdir -p "${TMP}/candidate" "${TMP}/base"
  export OUTPUT_DIR="${TMP}/candidate"
  printf '{"review test":{"status":"fail","exit_code":1,"structured_output":true}}' > "${TMP}/base/baseline-status.json"
}

# write_side DIR FAILED NAME... -- review-test.json with failure details plus
# the invalid per-test outcome sidecars Homeboy writes when the runtime plan
# cannot account for the failures.
write_side() {
  local dir="$1" failed="$2"
  shift 2
  python3 - "${dir}" "${failed}" "$@" <<'PY'
import json, sys
directory, failed, names = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
payload = {
    "status": "failed",
    "test_counts": {"total": 641, "passed": 641 - failed, "failed": failed, "skipped": 0},
    "summary": {"total": 641, "passed": 641 - failed, "failed": failed, "skipped": 0, "exit_code": 1,
                "failures": [{"test_name": name, "message": "boom"} for name in names]},
    "test_runtime_evidence": {"status": "invalid_evidence", "reason": "observed failed test ID is absent from the runtime plan"},
}
json.dump(payload, open(f"{directory}/review-test.json", "w"))
for suffix in ("test-outcomes", "test-inventory"):
    json.dump({"schema": f"homeboy/{suffix}/v1", "command": "review test",
               "invalid_evidence": {"type": "invalid_evidence", "reason": "observed failed test ID is absent from the runtime plan"}},
              open(f"{directory}/review-test.{suffix}.json", "w"))
PY
}

assert_contains() {
  [[ "$1" == *"$2"* ]] || { printf 'FAIL: expected %q in:\n%s\n' "$2" "$1"; exit 1; }
}
assert_not_contains() {
  [[ "$1" != *"$2"* ]] || { printf 'FAIL: did not expect %q in:\n%s\n' "$2" "$1"; exit 1; }
}

# 1. Failure details on both sides: name only the candidate-only failures.
reset_dirs
write_side "${TMP}/base" 2 'Suite::test_shared_a' 'Suite::test_shared_b'
write_side "${TMP}/candidate" 3 'Suite::test_shared_a' 'Suite::test_shared_b' 'Concert::test_new@flake'
RESULTS="$(python3 "${GATE}" '{"review test":"fail"}' "${OUTPUT_DIR}" "${TMP}/base" 2>/dev/null)"
[ "${RESULTS}" = '{"review test":"inconclusive"}' ] || { echo "FAIL: unexpected results ${RESULTS}"; exit 1; }
python3 - "${OUTPUT_DIR}" <<'PY'
import json, sys
verdict = json.load(open(f"{sys.argv[1]}/differential-comment.json"))["commands"]["review test"]
assert verdict["new_failed_tests"] == ["Concert::test_new@flake"], verdict
assert verdict["new_failed_tests_basis"] == "failure_details", verdict
assert "observed failed test ID is absent" in verdict["reason"], verdict
gate = json.load(open(f"{sys.argv[1]}/review-test.json"))["differential_gate"]
assert gate["new_failed_tests"] == ["Concert::test_new@flake"], gate
assert gate["status"] == "inconclusive", gate
PY
SECTION_BODY=''
build_section_body
assert_contains "${SECTION_BODY}" 'inconclusive (differential)'
assert_contains "${SECTION_BODY}" 'New failing tests (1, from failure details'
assert_contains "${SECTION_BODY}" "- \`Concert::test_new@"$'\u200b'"flake\`"
assert_not_contains "${SECTION_BODY}" 'Suite::test_shared_a'

# 2. More new failures than the cap: first 10, then "and N more".
reset_dirs
write_side "${TMP}/base" 0
names=()
for index in $(seq -w 1 13); do names+=("Suite::test_${index}"); done
write_side "${TMP}/candidate" 13 "${names[@]}"
printf '{"review test":{"status":"pass","exit_code":0,"structured_output":true}}' > "${TMP}/base/baseline-status.json"
python3 "${GATE}" '{"review test":"fail"}' "${OUTPUT_DIR}" "${TMP}/base" >/dev/null 2>&1
SECTION_BODY=''
build_section_body
assert_contains "${SECTION_BODY}" 'New failing tests (13'
assert_contains "${SECTION_BODY}" 'Suite::test_10'
assert_not_contains "${SECTION_BODY}" 'Suite::test_11'
assert_contains "${SECTION_BODY}" '…and 3 more'
python3 - "${OUTPUT_DIR}" <<'PY'
import json, sys
verdict = json.load(open(f"{sys.argv[1]}/differential-comment.json"))["commands"]["review test"]
assert len(verdict["new_failed_tests"]) == 13, verdict
PY

# 3. No usable names on either side: say why instead of a bare count.
reset_dirs
write_side "${TMP}/base" 16
write_side "${TMP}/candidate" 17
python3 "${GATE}" '{"review test":"fail"}' "${OUTPUT_DIR}" "${TMP}/base" >/dev/null 2>&1
python3 - "${OUTPUT_DIR}" <<'PY'
import json, sys
verdict = json.load(open(f"{sys.argv[1]}/differential-comment.json"))["commands"]["review test"]
assert "new_failed_tests" not in verdict, verdict
reason = verdict["new_failed_tests_unavailable_reason"]
assert "candidate per-test outcomes invalid (observed failed test ID is absent from the runtime plan)" in reason, reason
assert "candidate failure details name 0 of 17 failed test(s)" in reason, reason
gate = json.load(open(f"{sys.argv[1]}/review-test.json"))["differential_gate"]
assert gate["new_failed_tests_unavailable_reason"] == reason, gate
PY
SECTION_BODY=''
build_section_body
assert_contains "${SECTION_BODY}" '1 new finding(s)'
assert_contains "${SECTION_BODY}" '> New failing test IDs are unavailable: candidate per-test outcomes invalid'

# 4. A partial (capped) candidate list must not be diffed: an unnamed
# pre-existing failure would look new.
reset_dirs
write_side "${TMP}/base" 2 'Suite::test_a' 'Suite::test_b'
write_side "${TMP}/candidate" 3 'Suite::test_a' 'Suite::test_c'
python3 "${GATE}" '{"review test":"fail"}' "${OUTPUT_DIR}" "${TMP}/base" >/dev/null 2>&1
python3 - "${OUTPUT_DIR}" <<'PY'
import json, sys
verdict = json.load(open(f"{sys.argv[1]}/differential-comment.json"))["commands"]["review test"]
assert "new_failed_tests" not in verdict, verdict
assert "candidate failure details name 2 of 3 failed test(s)" in verdict["new_failed_tests_unavailable_reason"], verdict
PY

echo 'differential failed test id tests passed'
