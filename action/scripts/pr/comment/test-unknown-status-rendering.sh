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

body="$(RESULTS='{not-json' build_section_body; printf '%s\n' "${SECTION_BODY}")"
assert_contains "${body}" "core PR-comment rendering currently supports the default" "single-command runs use unsupported review warning"
assert_not_contains "${body}" ":warning: **audit**" "single-command runs do not render legacy warning block"
assert_not_contains "${body}" "Could not parse a pass/fail result for audit" "single-command runs do not render legacy parse warning"

body="$(RESULTS='{"audit":"mystery"}' build_section_body; printf '%s\n' "${SECTION_BODY}")"
assert_contains "${body}" "core PR-comment rendering currently supports the default" "unknown single-command status uses unsupported review warning"
assert_not_contains "${body}" ":warning: **audit**" "unknown status does not render legacy warning block"

body="$(RESULTS='{"audit":"fail"}' build_section_body; printf '%s\n' "${SECTION_BODY}")"
assert_contains "${body}" "core PR-comment rendering currently supports the default" "explicit single-command failure uses unsupported review warning"
assert_not_contains "${body}" ":x: **audit**" "explicit failure does not render legacy command failure"

printf 'All unsupported single-command rendering checks passed.\n'
