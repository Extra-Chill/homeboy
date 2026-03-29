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
OUTPUT_JSON="/tmp/workspace/out.json"

# ── Unscoped (full mode) ──
unset SCOPE_MODE SCOPE_BASE_REF EXTRA_ARGS || true
SCOPE_MODE="full"
assert_equals \
  "homeboy lint data-machine --path /tmp/workspace" \
  "$(build_run_command "lint" "${COMPONENT}" "${WORKSPACE}")" \
  "lint includes workspace path"

assert_equals \
  "homeboy --output /tmp/workspace/out.json lint data-machine --path /tmp/workspace" \
  "$(build_run_command "lint" "${COMPONENT}" "${WORKSPACE}" "${OUTPUT_JSON}")" \
  "lint includes structured output path"

# ── Scoped (changed mode) ──
SCOPE_MODE="changed"
SCOPE_BASE_REF="origin/main"
assert_equals \
  "homeboy lint data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "lint" "${COMPONENT}" "${WORKSPACE}")" \
  "lint keeps path with changed-since"

assert_equals \
  "homeboy --output /tmp/workspace/out.json lint data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "lint" "${COMPONENT}" "${WORKSPACE}" "${OUTPUT_JSON}")" \
  "lint keeps output path with changed-since"

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
  "homeboy --output /tmp/workspace/out.json audit data-machine --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_run_command "audit" "${COMPONENT}" "${WORKSPACE}" "${OUTPUT_JSON}")" \
  "run command keeps output path before extra args"

assert_equals \
  "homeboy refactor data-machine --from lint --write --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_autofix_command "refactor --from lint --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor lint keeps path and changed-since"

assert_equals \
  "homeboy --output /tmp/workspace/out.json refactor data-machine --from lint --write --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_autofix_command "refactor --from lint --write" "${COMPONENT}" "${WORKSPACE}" "${OUTPUT_JSON}")" \
  "autofix refactor lint keeps output path and changed-since"

assert_equals \
  "homeboy refactor data-machine --from audit --write --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_autofix_command "refactor --from audit --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor audit keeps path and changed-since"

assert_equals \
  "homeboy refactor data-machine --all --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_run_command "refactor --all" "${COMPONENT}" "${WORKSPACE}")" \
  "refactor keeps path with changed-since"

assert_equals \
  "refactor---all" \
  "$(command_output_stem "refactor --all")" \
  "output stem sanitizes spaced refactor command"

assert_equals \
  "refactor---from-audit---write" \
  "$(command_output_stem "refactor --from audit --write")" \
  "output stem sanitizes autofix command"

assert_equals \
  "homeboy refactor data-machine --from-audit --from-lint --from-test --all --write --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_autofix_command "refactor --from-audit --from-lint --from-test --all --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor keeps path and changed-since"

# ── Unscoped autofix ──
unset SCOPE_MODE SCOPE_BASE_REF EXTRA_ARGS || true
SCOPE_MODE="full"
assert_equals \
  "homeboy refactor data-machine --from test --write --path /tmp/workspace" \
  "$(build_autofix_command "refactor --from test --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor test keeps workspace path"

assert_equals \
  "homeboy refactor data-machine --all --path /tmp/workspace" \
  "$(build_run_command "refactor --all" "${COMPONENT}" "${WORKSPACE}")" \
  "refactor keeps workspace path"

PR_HEAD_REPO="some-contributor/homeboy-action"
GITHUB_REPOSITORY="Extra-Chill/homeboy-action"
GITHUB_HEAD_REF="feat/fork-pr"
GITHUB_REF_NAME="ignored-here"
assert_equals \
  "some-contributor/homeboy-action" \
  "$(resolve_pr_target_repo)" \
  "target repo prefers PR head repo"

assert_equals \
  "feat/fork-pr" \
  "$(resolve_pr_target_branch)" \
  "target branch prefers PR head ref"

assert_equals \
  "https://github.com/some-contributor/homeboy-action.git" \
  "$(build_github_remote_url "some-contributor/homeboy-action")" \
  "build github remote url without token"

assert_equals \
  "https://x-access-token:secret123@github.com/some-contributor/homeboy-action.git" \
  "$(build_github_remote_url "some-contributor/homeboy-action" "secret123")" \
  "build github remote url with token"

assert_equals \
  "origin" \
  "$(resolve_push_target "Extra-Chill/homeboy-action")" \
  "same-repo push without token uses origin"

assert_equals \
  "https://github.com/some-contributor/homeboy-action.git" \
  "$(resolve_push_target "some-contributor/homeboy-action")" \
  "fork push without token uses explicit remote url"

assert_equals \
  "https://x-access-token:secret123@github.com/some-contributor/homeboy-action.git" \
  "$(resolve_push_target "some-contributor/homeboy-action" "secret123")" \
  "fork push with token uses authenticated remote url"

# ── Canonicalize: fleet/deploy commands are filtered out ──

assert_equals \
  "audit,lint,test" \
  "$(canonicalize_commands "audit,lint,test,fleet exec my-fleet -- homeboy upgrade")" \
  "canonicalize strips fleet commands"

assert_equals \
  "audit,lint,test" \
  "$(canonicalize_commands "audit,deploy my-project --all,lint,test")" \
  "canonicalize strips deploy commands"

assert_equals \
  "audit,lint,test,refactor --all" \
  "$(canonicalize_commands "deploy --fleet prod data-machine,audit,lint,test,fleet status my-fleet,refactor --all")" \
  "canonicalize strips all operations and preserves order"

assert_equals \
  "" \
  "$(canonicalize_commands "fleet exec my-fleet -- homeboy upgrade,deploy my-project --all")" \
  "canonicalize returns empty when only operations commands"

printf 'All command builder checks passed.\n'
