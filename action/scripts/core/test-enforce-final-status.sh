#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENFORCE_STATUS="${SCRIPT_DIR}/enforce-final-status.sh"

assert_exit() {
  local expected="$1"
  local label="$2"
  shift 2

  local output status
  set +e
  output="$("$@" 2>&1)"
  status=$?
  set -e

  if [ "${status}" -ne "${expected}" ]; then
    printf 'FAIL: %s\nexpected exit: %s\nactual exit:   %s\noutput:        %s\n' "${label}" "${expected}" "${status}" "${output}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_exit 1 "malformed quality results fail closed" \
  env RESULTS='{"test":"fail"}}' COMMANDS='test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

assert_exit 1 "failing quality results fail" \
  env RESULTS='{"test":"fail"}' COMMANDS='test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

assert_exit 0 "passing quality results pass" \
  env RESULTS='{"test":"pass"}' COMMANDS='test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

printf 'All final status enforcement checks passed.\n'
