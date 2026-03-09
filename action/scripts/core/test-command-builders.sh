#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/lib.sh"

assert_equals() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual:   %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

WORKSPACE="/tmp/workspace"
COMPONENT="data-machine"

# ── Unscoped (full mode) ──
unset SCOPE_MODE SCOPE_BASE_REF EXTRA_ARGS || true
SCOPE_MODE="full"
assert_equals \
  "homeboy lint data-machine --path /tmp/workspace" \
  "$(build_run_command "lint" "${COMPONENT}" "${WORKSPACE}")" \
  "lint includes workspace path"

# ── Scoped (changed mode) ──
SCOPE_MODE="changed"
SCOPE_BASE_REF="origin/main"
assert_equals \
  "homeboy lint data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "lint" "${COMPONENT}" "${WORKSPACE}")" \
  "lint keeps path with changed-since"

assert_equals \
  "homeboy test data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "test" "${COMPONENT}" "${WORKSPACE}")" \
  "test keeps path with changed scope"

assert_equals \
  "homeboy audit data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "audit" "${COMPONENT}" "${WORKSPACE}")" \
  "audit keeps path with changed-since"

EXTRA_ARGS="--format json"
assert_equals \
  "homeboy audit data-machine --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_run_command "audit" "${COMPONENT}" "${WORKSPACE}")" \
  "run command appends extra args"

assert_equals \
  "homeboy refactor ci data-machine --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_run_command "refactor ci" "${COMPONENT}" "${WORKSPACE}")" \
  "refactor keeps path with changed-since"

assert_equals \
  "homeboy refactor ci --write data-machine --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_autofix_command "refactor ci --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor keeps path and changed-since"

# ── Unscoped autofix ──
unset SCOPE_MODE SCOPE_BASE_REF EXTRA_ARGS || true
SCOPE_MODE="full"
assert_equals \
  "homeboy refactor ci --write data-machine --path /tmp/workspace" \
  "$(build_autofix_command "refactor ci --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor keeps workspace path"

printf 'All command builder checks passed.\n'
