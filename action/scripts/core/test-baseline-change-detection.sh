#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/lib.sh"

assert_true() {
  local label="$1"
  shift

  if ! "$@" >/dev/null 2>&1; then
    printf 'FAIL: %s\n' "${label}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_false() {
  local label="$1"
  shift

  if "$@" >/dev/null 2>&1; then
    printf 'FAIL: %s\n' "${label}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

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

REPO="$(mktemp -d)"
trap 'rm -rf "${REPO}"' EXIT

git -C "${REPO}" init -q
git -C "${REPO}" config user.name test
git -C "${REPO}" config user.email test@example.com
printf '{"id":"root"}\n' > "${REPO}/homeboy.json"
mkdir -p "${REPO}/packages/plugin"
printf '{"id":"plugin"}\n' > "${REPO}/packages/plugin/homeboy.json"
printf 'initial\n' > "${REPO}/README.md"
git -C "${REPO}" add .
git -C "${REPO}" commit -q -m initial

cd "${REPO}"

printf '{"id":"root","audit":{"baseline":[]}}\n' > homeboy.json
assert_equals "homeboy.json" "$(baseline_file_path "${REPO}")" "root baseline path"
assert_true "root homeboy.json-only diff is baseline-only" changes_are_only_audit_baseline "${REPO}"

printf 'changed\n' > README.md
assert_false "source plus baseline diff is not baseline-only" changes_are_only_audit_baseline "${REPO}"

git checkout -q -- README.md homeboy.json
printf '{"id":"plugin","audit":{"baseline":[]}}\n' > packages/plugin/homeboy.json
assert_equals "packages/plugin/homeboy.json" "$(baseline_file_path "${REPO}/packages/plugin")" "component baseline path"
assert_true "component homeboy.json-only diff is baseline-only" changes_are_only_audit_baseline "${REPO}/packages/plugin"

printf 'All baseline change detection checks passed.\n'
