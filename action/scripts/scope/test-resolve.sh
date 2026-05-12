#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_SCRIPT="${SCRIPT_DIR}/resolve.sh"

assert_env_contains() {
  local file="$1"
  local expected="$2"
  local label="$3"

  if grep -qx "${expected}" "${file}"; then
    printf 'PASS: %s\n' "${label}"
    return
  fi

  printf 'FAIL: %s\nexpected env line: %s\nactual:\n' "${label}" "${expected}"
  sed 's/^/  /' "${file}"
  exit 1
}

run_resolve() {
  local env_file output_file
  env_file="$(mktemp)"
  output_file="$(mktemp)"

  GITHUB_ENV="${env_file}" \
    GITHUB_OUTPUT="${output_file}" \
    "$@" \
    bash "${RESOLVE_SCRIPT}" >/dev/null

  printf '%s\n' "${env_file}"
}

env_file="$(run_resolve env GITHUB_EVENT_NAME=push SCOPE_INPUT=auto)"
assert_env_contains "${env_file}" "SCOPE_MODE=full" "auto push uses full scope"

env_file="$(run_resolve env GITHUB_EVENT_NAME=pull_request SCOPE_INPUT=full BASE_SHA=missing)"
assert_env_contains "${env_file}" "SCOPE_MODE=full" "explicit full scope bypasses PR base resolution"

env_file="$(run_resolve env GITHUB_EVENT_NAME=workflow_dispatch SCOPE_INPUT=changed)"
assert_env_contains "${env_file}" "SCOPE_MODE=full" "changed scope without base falls back to full"

env_file="$(run_resolve env GITHUB_EVENT_NAME=push SCOPE_INPUT=unexpected)"
assert_env_contains "${env_file}" "SCOPE_MODE=full" "invalid scope falls back to auto"

base_sha="$(git rev-parse HEAD~1)"
env_file="$(run_resolve env GITHUB_EVENT_NAME=pull_request SCOPE_INPUT=auto BASE_SHA="${base_sha}" PR_HEAD_REPO=Extra-Chill/homeboy-action GITHUB_REPOSITORY=Extra-Chill/homeboy-action)"
assert_env_contains "${env_file}" "SCOPE_MODE=changed" "auto PR uses changed scope"
assert_env_contains "${env_file}" "SCOPE_BASE_REF=${base_sha}" "auto PR records base ref"

printf 'All scope resolver checks passed.\n'
