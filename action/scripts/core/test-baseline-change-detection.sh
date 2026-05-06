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
printf '# placeholder lockfile\n' > "${REPO}/Cargo.lock"
mkdir -p "${REPO}/packages/plugin"
printf '{"id":"plugin"}\n' > "${REPO}/packages/plugin/homeboy.json"
printf 'initial\n' > "${REPO}/README.md"
git -C "${REPO}" add .
git -C "${REPO}" commit -q -m initial

cd "${REPO}"

# baseline_file_path keeps emitting the homeboy.json path for legacy callers.
assert_equals "homeboy.json" "$(baseline_file_path "${REPO}")" "root baseline path"
assert_equals "packages/plugin/homeboy.json" "$(baseline_file_path "${REPO}/packages/plugin")" "component baseline path"

# drift_file_paths includes Cargo.lock when one exists at the repo root.
expected_drift="$(printf 'homeboy.json\nCargo.lock\n')"
assert_equals "${expected_drift}" "$(drift_file_paths "${REPO}")" "root drift list includes Cargo.lock"

# Component workspaces share the root Cargo.lock — only the component's
# homeboy.json is component-scoped.
expected_drift_component="$(printf 'packages/plugin/homeboy.json\nCargo.lock\n')"
assert_equals "${expected_drift_component}" "$(drift_file_paths "${REPO}/packages/plugin")" "component drift list includes root Cargo.lock"

# homeboy.json-only diff is drift-only.
printf '{"id":"root","audit":{"baseline":[]}}\n' > homeboy.json
assert_true "root homeboy.json-only diff is drift-only" changes_are_only_drift "${REPO}"
assert_true "legacy alias accepts homeboy.json-only diff" changes_are_only_audit_baseline "${REPO}"

# Cargo.lock-only diff is drift-only.
git checkout -q -- homeboy.json
printf '# bumped lockfile\n' > Cargo.lock
assert_true "Cargo.lock-only diff is drift-only" changes_are_only_drift "${REPO}"

# homeboy.json + Cargo.lock together is still drift-only.
printf '{"id":"root","audit":{"baseline":[]}}\n' > homeboy.json
assert_true "homeboy.json + Cargo.lock diff is drift-only" changes_are_only_drift "${REPO}"

# Source change alongside drift breaks the drift-only invariant.
printf 'changed\n' > README.md
assert_false "source plus drift diff is not drift-only" changes_are_only_drift "${REPO}"

git checkout -q -- README.md homeboy.json Cargo.lock

# Component-only baseline change is still drift-only.
printf '{"id":"plugin","audit":{"baseline":[]}}\n' > packages/plugin/homeboy.json
assert_true "component homeboy.json-only diff is drift-only" changes_are_only_drift "${REPO}/packages/plugin"

git checkout -q -- packages/plugin/homeboy.json

# Repos without a Cargo.lock get a single-file drift list.
NON_RUST_REPO="$(mktemp -d)"
git -C "${NON_RUST_REPO}" init -q
git -C "${NON_RUST_REPO}" config user.name test
git -C "${NON_RUST_REPO}" config user.email test@example.com
printf '{"id":"php"}\n' > "${NON_RUST_REPO}/homeboy.json"
printf 'README\n' > "${NON_RUST_REPO}/README.md"
git -C "${NON_RUST_REPO}" add .
git -C "${NON_RUST_REPO}" commit -q -m initial

assert_equals "homeboy.json" "$(drift_file_paths "${NON_RUST_REPO}")" "non-Rust repo drift list omits Cargo.lock"
rm -rf "${NON_RUST_REPO}"

printf 'All baseline change detection checks passed.\n'
