#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"

  if [[ "${haystack}" != *"${needle}"* ]]; then
    printf 'FAIL: %s\nmissing: %s\noutput:\n%s\n' "${label}" "${needle}" "${haystack}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_equals() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual: %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

FAKE_BIN="${TMP_DIR}/bin"
mkdir -p "${FAKE_BIN}" "${TMP_DIR}/output"

cat > "${FAKE_BIN}/gh" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == *"/comments"* ]]; then
  printf '%s\n' "${EXISTING_COMMENTS:-[]}"
else
  printf 'OPEN\n'
fi
SH
chmod +x "${FAKE_BIN}/gh"

cat > "${FAKE_BIN}/homeboy" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${HOMEBOY_CALL_LOG}"
printf '{"action":"created","data":{"comment_id":123}}\n'
SH
chmod +x "${FAKE_BIN}/homeboy"

export PATH="${FAKE_BIN}:${PATH}"
export GITHUB_ACTION_PATH="${ROOT}"
export HOMEBOY_OUTPUT_DIR="${TMP_DIR}/output"
export HOMEBOY_CALL_LOG="${TMP_DIR}/homeboy.log"
export GITHUB_ENV="${TMP_DIR}/github.env"
export GITHUB_STEP_SUMMARY="${TMP_DIR}/summary"
export GITHUB_REPOSITORY="Extra-Chill/homeboy-action"
export GITHUB_WORKFLOW="Homeboy"
export GITHUB_JOB="homeboy"
export GH_TOKEN="app-token"
export PR_NUMBER="191"
export COMPONENT_NAME="homeboy-action"
export COMMANDS="review lint"
export BINARY_SOURCE="source"
export SCOPE_MODE="full"
export TEST_SCOPE_EFFECTIVE="full"
export HOMEBOY_CLI_VERSION="homeboy 1.2.3"
export HOMEBOY_EXTENSION_ID="auto"
export HOMEBOY_EXTENSION_SOURCE="auto"
export HOMEBOY_EXTENSION_REVISION="abc1234"
export HOMEBOY_ACTION_REPOSITORY="Extra-Chill/homeboy-action"
export HOMEBOY_ACTION_REF="fix-skip-green-noop-comments"

run_post_comment() {
  : > "${HOMEBOY_CALL_LOG}"
  : > "${GITHUB_ENV}"
  bash "${ROOT}/scripts/pr/post-pr-comment.sh"
}

output="$(RESULTS='{"review lint":"pass"}' run_post_comment)"
assert_contains "${output}" "Skipping PR comment — run passed with no actionable content" "clean run is skipped"
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "clean run does not call homeboy comment primitive"

test -s "${GITHUB_STEP_SUMMARY}"
export EXISTING_COMMENTS='[{"body":"<!-- homeboy:comment-key=Homeboy:homeboy-action -->"}]'
output="$(RESULTS='{"review lint":"pass"}' run_post_comment)"
assert_contains "${output}" "PR comment posted successfully" "green resolves existing section"
unset EXISTING_COMMENTS
printf '%s' '{"schema":"homeboy/differential-comment/v1","commands":{"review lint":{"status":"fail","new_findings":1,"new_finding_details":[{"file":"src/a,b:c.php","line":7,"rule":"rule,:","severity":"error","message":"bad @user%\r\nline","fingerprint":"new"}]}}}' > "${HOMEBOY_OUTPUT_DIR}/differential-comment.json"
output="$(RESULTS='{"review lint":"fail"}' run_post_comment)"
assert_contains "${output}" 'file=src/a%2Cb%3Ac.php,line=7,title=rule%2C%3A::bad' "escaped annotation"
assert_contains "${output}" '%25%0D%0Aline' "escaped annotation message"
assert_contains "${output}" "PR comment posted successfully" "failed run comments"
assert_equals "1" "$(wc -l < "${HOMEBOY_CALL_LOG}" | xargs)" "failed run posts section with tooling footer"
assert_contains "$(cat "${HOMEBOY_CALL_LOG}")" "--footer-file" "failed run delegates tooling footer to core"
assert_contains "$(cat "${HOMEBOY_CALL_LOG}")" "--section-key review-lint" "failed run replaces the current command section rather than retaining stale green content"

rm "${HOMEBOY_OUTPUT_DIR}/differential-comment.json"
output="$(RESULTS='{"review lint":"inconclusive"}' run_post_comment)"
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "inconclusive never posts"
output="$(PR_COMMENT_INPUT=off RESULTS='{"review lint":"fail"}' run_post_comment)"
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "off never posts"
output="$(PR_COMMENT_INPUT=always RESULTS='{"review lint":"mystery"}' run_post_comment)"
assert_contains "${output}" "PR comment posted successfully" "unknown-status run comments"
assert_equals "1" "$(wc -l < "${HOMEBOY_CALL_LOG}" | xargs)" "unknown-status run posts section with tooling footer"
assert_contains "$(cat "${HOMEBOY_CALL_LOG}")" "--footer-file" "unknown-status run delegates tooling footer to core"

printf 'All green no-op PR comment checks passed.\n'
