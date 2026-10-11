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
export COMMANDS="review lint"
export COMP_ID="data-machine"
export WORKSPACE="/tmp/workspace"
export SECTION_TITLE="Lint"
export BINARY_SOURCE="source"
export SCOPE_MODE="full"
export DIGEST_FILE=""
export OUTPUT_DIR=""

source "${ROOT}/scripts/core/lib.sh"
source "${ROOT}/scripts/pr/comment/sections.sh"

body="$(RESULTS='{not-json' build_section_body; printf '%s\n' "${SECTION_BODY}")"
assert_contains "${body}" ":warning: **review lint** — unknown" "invalid results render unknown status"
assert_contains "${body}" "Structured output for \`review lint\` was not found" "missing lint json warning rendered"
assert_not_contains "${body}" "Could not parse a pass/fail result for lint" "single-command runs do not render legacy parse warning"

body="$(RESULTS='{"review lint":"mystery"}' build_section_body; printf '%s\n' "${SECTION_BODY}")"
assert_contains "${body}" ":warning: **review lint** — unknown" "unknown single-command status renders unknown"
assert_contains "${body}" "Structured output for \`review lint\` was not found" "unknown status missing json warning rendered"

body="$(RESULTS='{"review lint":"fail"}' build_section_body; printf '%s\n' "${SECTION_BODY}")"
assert_contains "${body}" ":x: **review lint** — failed" "explicit single-command failure renders failure"
assert_contains "${body}" "Structured output for \`review lint\` was not found" "failed status missing json warning rendered"

printf 'All unsupported single-command rendering checks passed.\n'
