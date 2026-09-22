#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ACTION="${ROOT_DIR}/action.yml"
WORKFLOW="${ROOT_DIR}/.github/workflows/release.yml"
SECRET_VALUE='npm-secret-value-must-not-be-printed'

resolve_token() {
  local dry_run="$1"
  local token="$2"
  if [ "${dry_run}" = "true" ]; then
    printf '%s' ''
  else
    printf '%s' "${token}"
  fi
}

assert_equal() {
  local expected="$1"
  local actual="$2"
  local label="$3"
  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual: %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

assert_fixed_contains() {
  local needle="$1"
  local file_path="$2"
  local label="$3"
  if ! grep -Fq -- "${needle}" "${file_path}"; then
    printf 'FAIL: %s\nmissing: %s\nfile: %s\n' "${label}" "${needle}" "${file_path}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

assert_fixed_not_contains() {
  local needle="$1"
  local file_path="$2"
  local label="$3"
  if grep -Fq -- "${needle}" "${file_path}"; then
    printf 'FAIL: %s\nfound: %s\nfile: %s\n' "${label}" "${needle}" "${file_path}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

assert_equal "${SECRET_VALUE}" "$(resolve_token false "${SECRET_VALUE}")" "credential-present release receives the token"
assert_equal '' "$(resolve_token false '')" "credential-absent release preserves the OIDC path"
assert_equal '' "$(resolve_token true "${SECRET_VALUE}")" "dry-run receives no token"

assert_fixed_contains "NPM_TOKEN: \${{ inputs.release-dry-run != 'true' && inputs.npm-token || '' }}" "${ACTION}" "action scopes token to non-dry-run release"
assert_fixed_contains "NPM_CONFIG_PROVENANCE: \${{ inputs.release-dry-run != 'true' && inputs.npm-token == '' && 'true' || '' }}" "${ACTION}" "action enables provenance for tokenless release"
assert_fixed_contains "npm-token: \${{ inputs.dry-run != true && secrets.NPM_TOKEN || '' }}" "${WORKFLOW}" "reusable workflow withholds token during dry-run"
assert_fixed_not_contains 'npm-token.*GITHUB_OUTPUT' "${ACTION}" "token is not emitted as an output"
assert_fixed_not_contains "${SECRET_VALUE}" "${ACTION}" "secret values are absent from action source"
assert_fixed_not_contains "${SECRET_VALUE}" "${WORKFLOW}" "secret values are absent from workflow source"

printf 'PASS: npm credential contract emits no secret value\n'
