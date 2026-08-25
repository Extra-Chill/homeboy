#!/usr/bin/env bash
#
# Tests for scripts/issues/auto-file-categorized-issues.sh empty-input handling.
#
# Covers issue #214: the categorizer must distinguish "nothing to categorize"
# (success) from "a command failed before producing output" (failure).
#
# These tests do not exercise the reconcile path against a real `homeboy`
# binary — they target the decision logic at the tail of the script that
# determines the final exit code when zero commands produced structured output.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="${SCRIPT_DIR}/auto-file-categorized-issues.sh"

if [ ! -f "${TARGET}" ]; then
  printf 'FAIL: target script not found at %s\n' "${TARGET}" >&2
  exit 1
fi

TMP_ROOT=$(mktemp -d)
trap 'rm -rf "${TMP_ROOT}"' EXIT

PASS_COUNT=0
FAIL_COUNT=0

# run_categorizer LABEL EXPECTED_EXIT EXPECTED_SUBSTRING ENV_KEY=VAL ...
#
# Runs the categorizer with the given env, asserts the exit code, and asserts
# the output contains EXPECTED_SUBSTRING (set to '' to skip the substring check).
#
# A throwaway HOMEBOY_OUTPUT_DIR is provided so the script's per-command
# json-file probe finds nothing and falls through to the empty-input handler
# we are testing. GITHUB_REPOSITORY / GITHUB_SERVER_URL / GITHUB_RUN_ID are
# stubbed so the unconditional `set -u` references succeed.
run_categorizer() {
  local label="$1"
  local expected_exit="$2"
  local expected_substring="$3"
  shift 3

  local case_dir
  case_dir=$(mktemp -d "${TMP_ROOT}/case.XXXXXX")
  local output_dir="${case_dir}/output"
  mkdir -p "${output_dir}"

  local output status
  set +e
  output=$(env -i \
    PATH="${PATH}" \
    HOME="${HOME}" \
    GITHUB_REPOSITORY="example-org/example-repo" \
    GITHUB_SERVER_URL="https://github.com" \
    GITHUB_RUN_ID="0" \
    GITHUB_WORKSPACE="${case_dir}" \
    HOMEBOY_OUTPUT_DIR="${output_dir}" \
    COMPONENT_NAME="example" \
    "$@" \
    bash "${TARGET}" 2>&1)
  status=$?
  set -e

  local fail=false
  if [ "${status}" -ne "${expected_exit}" ]; then
    fail=true
  fi

  if [ -n "${expected_substring}" ] && ! printf '%s\n' "${output}" | grep -qF -- "${expected_substring}"; then
    fail=true
  fi

  if [ "${fail}" = true ]; then
    FAIL_COUNT=$((FAIL_COUNT + 1))
    printf 'FAIL: %s\n' "${label}"
    printf '  expected exit:    %s\n' "${expected_exit}"
    printf '  actual exit:      %s\n' "${status}"
    if [ -n "${expected_substring}" ]; then
      printf '  expected substr:  %s\n' "${expected_substring}"
    fi
    printf '  output:\n'
    printf '    %s\n' "${output}" | sed 's/^/    /'
    return
  fi

  PASS_COUNT=$((PASS_COUNT + 1))
  printf 'PASS: %s\n' "${label}"
}

# ─────────────────────────────────────────────────────────────────────────────
# Issue #214: empty-input branches
# ─────────────────────────────────────────────────────────────────────────────

cat > "${TMP_ROOT}/setup.json" <<'JSON'
{"schema":"homeboy/action-setup-result/v1","phase":"dependency_build_setup","status":"failed","owner":"Homeboy extension setup","step":"install Homeboy extension","exit_code":1,"replay_command":"bash install-extension.sh","diagnostic":"source SHA mismatch"}
JSON

run_categorizer \
  "setup-blocked commands preserve setup ownership" \
  0 \
  "Homeboy extension setup failed during install Homeboy extension; requested quality commands were not run" \
  RESULTS='{"setup":"fail","review lint":"not_run"}' \
  COMMANDS='review lint' \
  HOMEBOY_SETUP_RESULT_FILE="${TMP_ROOT}/setup.json"

# Case 1 — release-wrapper shape: RESULTS={}, COMMANDS empty, EXPECTED set.
# This is the exact failure mode from the bug report. Should exit 0.
run_categorizer \
  "release-wrapper empty inputs exit 0" \
  0 \
  "Nothing to categorize" \
  RESULTS='{}' \
  COMMANDS='' \
  EXPECTED_COMMANDS='review audit,review lint,review test'

# Case 2 — totally empty env (no RESULTS at all). Should exit 0.
run_categorizer \
  "missing RESULTS exits 0" \
  0 \
  "Nothing to categorize" \
  RESULTS='' \
  COMMANDS='' \
  EXPECTED_COMMANDS=''

# Case 3 — RESULTS marks a command as failed. Categorizer can't file findings,
# but must propagate the failure so the release pipeline doesn't proceed.
run_categorizer \
  "failed command in RESULTS propagates exit 1" \
  1 \
  "No structured result exists for review audit; attempted" \
  RESULTS='{"review audit":"fail"}' \
  COMMANDS='review audit' \
  EXPECTED_COMMANDS='review audit,review lint,review test'

# Case 4 — RESULTS marks all commands as passing but no JSON files exist.
# This is the "clean codebase" path: nothing to categorize, exit 0.
run_categorizer \
  "all commands passed with no findings exits 0" \
  0 \
  "Nothing to categorize" \
  RESULTS='{"review audit":"pass","review lint":"pass","review test":"pass"}' \
  COMMANDS='review audit,review lint,review test' \
  EXPECTED_COMMANDS='review audit,review lint,review test'

# Case 5 — Malformed RESULTS JSON. Should not crash; treats as empty and
# exits 0 with a warning so release pipelines aren't blocked by env quirks.
run_categorizer \
  "malformed RESULTS treated as empty (exit 0 with warning)" \
  0 \
  "RESULTS is not a valid JSON object" \
  RESULTS='not-json' \
  COMMANDS='' \
  EXPECTED_COMMANDS='review audit,review lint,review test'

# ─────────────────────────────────────────────────────────────────────────────
# Summary
# ─────────────────────────────────────────────────────────────────────────────

printf '\n%s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"

if [ "${FAIL_COUNT}" -ne 0 ]; then
  exit 1
fi
