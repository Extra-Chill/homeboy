#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"

  if [[ "${haystack}" != *"${needle}"* ]]; then
    printf 'FAIL: %s\nmissing: %s\nbody:\n%s\n' "${label}" "${needle}" "${haystack}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_not_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"

  if [[ "${haystack}" == *"${needle}"* ]]; then
    printf 'FAIL: %s\nunexpected: %s\nbody:\n%s\n' "${label}" "${needle}" "${haystack}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

render_for_results() {
  export RESULTS="$1"
  build_section_body
  printf '%s\n' "${SECTION_BODY}"
}

export GITHUB_ACTION_PATH="${ROOT}"
export COMMANDS="audit"
export COMP_ID="data-machine"
export WORKSPACE="/tmp/workspace"
export SECTION_TITLE="Audit"
export AUTOFIX_ENABLED="false"
export BINARY_SOURCE="source"
export SCOPE_MODE="full"
export DIGEST_FILE=""

source "${ROOT}/scripts/core/lib.sh"
source "${ROOT}/scripts/pr/comment/sections.sh"

body="$(render_for_results '{not-json')"
assert_contains "${body}" ":warning: **audit**" "malformed results render as warning"
assert_contains "${body}" "Could not parse a pass/fail result for audit" "malformed results explain parse problem"
assert_not_contains "${body}" ":x: **audit**" "malformed results do not render as command failure"

body="$(render_for_results '{"audit":"mystery"}')"
assert_contains "${body}" ":warning: **audit**" "unknown status renders as warning"
assert_contains "${body}" "Could not parse a pass/fail result for audit" "unknown status explains parse problem"
assert_not_contains "${body}" ":x: **audit**" "unknown status does not render as command failure"

body="$(render_for_results '{"audit":"fail"}')"
assert_contains "${body}" ":x: **audit**" "explicit failure remains failure"
assert_not_contains "${body}" "Could not parse a pass/fail result for audit" "explicit failure has no parse warning"

printf 'All unknown status rendering checks passed.\n'
