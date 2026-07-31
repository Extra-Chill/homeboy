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
  env RESULTS='{"review test":"fail"}}' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

assert_exit 1 "failing quality results fail" \
  env RESULTS='{"review test":"fail"}' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

assert_exit 1 "timed out quality results fail with their classification" \
  env RESULTS='{"review test":"timeout"}' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

assert_exit 1 "missing quality results with expected commands fail closed" \
  env RESULTS='' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

assert_exit 0 "closed PR ignores stale failing quality results" \
  env RESULTS='{"review test":"fail"}' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='false' bash "${ENFORCE_STATUS}"

assert_exit 0 "closed PR ignores malformed stale results" \
  env RESULTS='{"review test":"fail"}}' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='false' bash "${ENFORCE_STATUS}"

assert_exit 0 "passing quality results pass" \
  env RESULTS='{"review test":"pass"}' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

assert_exit 0 "baseline red quality results do not fail PR" \
  env RESULTS='{"review test":"baseline_red"}' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

assert_exit 0 "inconclusive quality results do not fail PR" \
  env RESULTS='{"review lint":"inconclusive"}' COMMANDS='review lint' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

# `no_measurement` is non-blocking by design: a candidate is not answerable for
# an infrastructure condition it did not cause. This pins that, so the split
# introduced for Extra-Chill/homeboy#10999 cannot quietly start failing PRs.
assert_exit 0 "no_measurement quality results do not fail PR" \
  env RESULTS='{"review test":"no_measurement"}' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}"

# ...and it must still say so out loud. A silent non-blocking verdict is how an
# unmeasured command reads as a clean run.
output="$(env RESULTS='{"review test":"no_measurement"}' COMMANDS='review test' OPERATIONS_RESULTS='' PR_ACTIVE='' bash "${ENFORCE_STATUS}" 2>&1)"
case "${output}" in
  *"::warning::"*"no measurement"*) printf 'PASS: no_measurement emits a warning annotation\n' ;;
  *) printf 'FAIL: no_measurement did not annotate; got: %s\n' "${output}"; exit 1 ;;
esac

printf 'All final status enforcement checks passed.\n'
