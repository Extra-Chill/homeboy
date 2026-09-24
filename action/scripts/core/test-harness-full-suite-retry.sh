#!/usr/bin/env bash

# Extra-Chill/homeboy-action#507: a workflow- or config-only PR that changes a
# test-harness config file (`.github/workflows/**`, `homeboy.json`,
# phpunit/jest/vitest/playwright configs) makes Homeboy's changed-scope Test
# phase select zero tests and fail closed with finding
# `changed_scope_zero_tests_for_harness_change`, because a zero-test run
# cannot prove the harness change still works. This action must re-run that
# Test phase without --changed-since (the full suite) and pass when it is
# green, instead of forcing every workflow-only PR to merge red.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

# ── Unit tests for the lib.sh helpers ──

mkdir -p "${TMP_DIR}/bin"
cat > "${TMP_DIR}/bin/homeboy" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "${TMP_DIR}/bin/homeboy"
export PATH="${TMP_DIR}/bin:${PATH}"
# shellcheck disable=SC1091
source "${ROOT_DIR}/scripts/core/lib.sh"

assert_true() {
  local label="$1"
  shift
  if "$@"; then
    printf 'PASS: %s\n' "${label}"
  else
    printf 'FAIL: %s\n' "${label}"
    exit 1
  fi
}

assert_false() {
  local label="$1"
  shift
  if "$@"; then
    printf 'FAIL: %s\n' "${label}"
    exit 1
  else
    printf 'PASS: %s\n' "${label}"
  fi
}

with_harness_finding="${TMP_DIR}/with-harness-finding.json"
cat > "${with_harness_finding}" <<'JSON'
{"schema":"homeboy/command-result/v3","command":"review","operation":"test","success":false,"status":"failed","exit_code":1,"data":{"findings":[{"tool":"test","rule":"changed_scope_zero_tests_for_harness_change","message":"zero tests selected"}]}}
JSON
assert_true "envelope with the harness finding is detected" \
  command_result_has_harness_zero_test_finding "${with_harness_finding}"

with_other_finding="${TMP_DIR}/with-other-finding.json"
cat > "${with_other_finding}" <<'JSON'
{"schema":"homeboy/command-result/v3","command":"review","operation":"test","success":false,"status":"failed","exit_code":1,"data":{"findings":[{"tool":"test","rule":"changed_scope_zero_tests_for_source_change","message":"zero tests selected"}]}}
JSON
assert_false "envelope with a different finding rule is not detected" \
  command_result_has_harness_zero_test_finding "${with_other_finding}"

without_findings="${TMP_DIR}/without-findings.json"
cat > "${without_findings}" <<'JSON'
{"schema":"homeboy/command-result/v3","command":"review","operation":"test","success":true,"status":"succeeded","exit_code":0,"data":{"test_counts":{"failed":0,"passed":5,"total":5}}}
JSON
assert_false "envelope with no findings is not detected" \
  command_result_has_harness_zero_test_finding "${without_findings}"

bare_data="${TMP_DIR}/bare-data.json"
cat > "${bare_data}" <<'JSON'
{"findings":[{"tool":"test","rule":"changed_scope_zero_tests_for_harness_change","message":"zero tests selected"}]}
JSON
assert_true "bare (un-enveloped) test-double payload is still detected" \
  command_result_has_harness_zero_test_finding "${bare_data}"

missing_file="${TMP_DIR}/does-not-exist.json"
assert_false "a missing output file is not detected" \
  command_result_has_harness_zero_test_finding "${missing_file}"

assert_true "comma_list_contains finds a trimmed member" \
  comma_list_contains 'review test, review lint' 'review lint'
assert_false "comma_list_contains rejects an absent member" \
  comma_list_contains 'review lint,review audit' 'review test'
assert_false "comma_list_contains rejects an empty haystack" \
  comma_list_contains '' 'review test'

printf '\n'

# ── Integration: run-homeboy-commands.sh retries the full suite ──

mkdir -p "${TMP_DIR}/bin" "${TMP_DIR}/workspace"
cat > "${TMP_DIR}/bin/homeboy" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

output=""
scoped=false
args=("$@")
for arg in "${args[@]}"; do
  case "${arg}" in
    --changed-since) scoped=true ;;
  esac
done
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    *) shift ;;
  esac
done
mkdir -p "$(dirname "${output}")"

if [ "${scoped}" = true ]; then
  printf '%s\n' '{"schema":"homeboy/command-result/v3","command":"review","operation":"test","success":false,"status":"failed","exit_code":1,"data":{"findings":[{"tool":"test","rule":"changed_scope_zero_tests_for_harness_change","message":"changed-scope test gate selected zero tests, but 1 test-harness config file changed"}],"hints":["Run the full suite so the harness change is exercised: homeboy review test fixture"]}}' > "${output}"
  exit 1
fi

printf '%s\n' '{"schema":"homeboy/command-result/v3","command":"review","operation":"test","success":true,"status":"succeeded","exit_code":0,"data":{"test_counts":{"failed":0,"passed":12,"total":12}}}' > "${output}"
exit 0
SH
chmod +x "${TMP_DIR}/bin/homeboy"

run_it() {
  local label="$1"
  shift
  local env_file="${TMP_DIR}/github-env-${label}"
  local out_file="${TMP_DIR}/github-output-${label}"
  local log_file="${TMP_DIR}/run-${label}.log"

  # Extra per-call env assignments ("$@" here) are dynamic words, so bash's
  # lexical assignment-prefix parsing never recognizes them -- they must go
  # through `env`, not a bare `VAR=val` prefix.
  set +e
  env \
  PATH="${TMP_DIR}/bin:${PATH}" \
  GITHUB_ACTION_PATH="${ROOT_DIR}" \
  GITHUB_WORKSPACE="${TMP_DIR}/workspace-${label}" \
  GITHUB_OUTPUT="${out_file}" \
  GITHUB_ENV="${env_file}" \
  RESOLVED_COMMANDS='review test' \
  COMPONENT_NAME='fixture' \
  "$@" \
  bash "${ROOT_DIR}/scripts/core/run-homeboy-commands.sh" >"${log_file}" 2>&1
  local exit_code=$?
  set -e
  printf '%s\n' "${exit_code}"
}

exit_code="$(run_it harness-scoped SCOPE_MODE=changed SCOPE_BASE_REF=abc123base)"
if [ "${exit_code}" -ne 0 ]; then
  printf 'FAIL: harness-only scoped Test phase does not pass after the full-suite retry (exit %s)\n' "${exit_code}"
  cat "${TMP_DIR}/run-harness-scoped.log"
  exit 1
fi
if ! grep -q '^results={"review test":"pass"}$' "${TMP_DIR}/github-output-harness-scoped"; then
  printf 'FAIL: harness-only scoped Test phase is not recorded as pass after retry\n'
  cat "${TMP_DIR}/run-harness-scoped.log"
  exit 1
fi
if ! grep -q 'Re-running (full suite, harness change)' "${TMP_DIR}/run-harness-scoped.log"; then
  printf 'FAIL: full-suite retry is not announced in the run log\n'
  exit 1
fi
if ! grep -q '^HOMEBOY_HARNESS_FULL_SUITE_RETRY_COMMANDS=review test$' "${TMP_DIR}/github-env-harness-scoped"; then
  printf 'FAIL: escalated command is not recorded for the baseline step\n'
  cat "${TMP_DIR}/github-env-harness-scoped"
  exit 1
fi
printf 'PASS: a scoped, harness-only Test phase retries the full suite and passes when it is green\n'

# Unscoped (full-scope, e.g. push/cron) runs never pass --changed-since, so the
# fake homeboy above always takes its unscoped branch and passes on the first
# try; nothing here should claim a retry happened.
exit_code="$(run_it unscoped SCOPE_MODE=full)"
if [ "${exit_code}" -ne 0 ]; then
  printf 'FAIL: unscoped Test phase unexpectedly failed (exit %s)\n' "${exit_code}"
  cat "${TMP_DIR}/run-unscoped.log"
  exit 1
fi
if grep -q 'Re-running (full suite, harness change)' "${TMP_DIR}/run-unscoped.log"; then
  printf 'FAIL: an already-unscoped Test phase should never retry\n'
  exit 1
fi
if grep -q 'HOMEBOY_HARNESS_FULL_SUITE_RETRY_COMMANDS' "${TMP_DIR}/github-env-unscoped"; then
  printf 'FAIL: an already-unscoped Test phase must not mark a retry for the baseline step\n'
  exit 1
fi
printf 'PASS: an already-unscoped Test phase never retries\n'

# A source-only PR (real, non-harness failure) must keep using changed-scope
# selection and must not be retried unscoped.
cat > "${TMP_DIR}/bin/homeboy-source-fail" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    *) shift ;;
  esac
done
mkdir -p "$(dirname "${output}")"
printf '%s\n' '{"schema":"homeboy/command-result/v3","command":"review","operation":"test","success":false,"status":"failed","exit_code":1,"data":{"findings":[{"tool":"test","rule":"changed_scope_zero_tests_for_source_change","message":"zero tests selected, but source changed"}]}}' > "${output}"
exit 1
SH
mkdir -p "${TMP_DIR}/bin-source-fail"
cp "${TMP_DIR}/bin/homeboy-source-fail" "${TMP_DIR}/bin-source-fail/homeboy"
chmod +x "${TMP_DIR}/bin-source-fail/homeboy"

set +e
PATH="${TMP_DIR}/bin-source-fail:${PATH}" \
GITHUB_ACTION_PATH="${ROOT_DIR}" \
GITHUB_WORKSPACE="${TMP_DIR}/workspace-source-fail" \
GITHUB_OUTPUT="${TMP_DIR}/github-output-source-fail" \
GITHUB_ENV="${TMP_DIR}/github-env-source-fail" \
RESOLVED_COMMANDS='review test' \
COMPONENT_NAME='fixture' \
SCOPE_MODE=changed \
SCOPE_BASE_REF=abc123base \
bash "${ROOT_DIR}/scripts/core/run-homeboy-commands.sh" >"${TMP_DIR}/run-source-fail.log" 2>&1
exit_code=$?
set -e

if [ "${exit_code}" -ne 1 ] || ! grep -q '^results={"review test":"fail"}$' "${TMP_DIR}/github-output-source-fail"; then
  printf 'FAIL: a source-change zero-test finding must still fail the gate\n'
  cat "${TMP_DIR}/run-source-fail.log"
  exit 1
fi
if grep -q 'Re-running (full suite, harness change)' "${TMP_DIR}/run-source-fail.log"; then
  printf 'FAIL: changed_scope_zero_tests_for_source_change must never trigger the harness retry\n'
  exit 1
fi
printf 'PASS: a source-only zero-test finding keeps using changed-scope selection and stays failed\n'
