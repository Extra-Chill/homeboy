#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/release.yml"
RUN_RELEASE="${ROOT_DIR}/scripts/release/run-release.sh"

assert_not_contains() {
  local needle="$1"
  local file_path="$2"
  local label="$3"

  if grep -q -- "${needle}" "${file_path}"; then
    printf 'FAIL: %s\nfound forbidden pattern: %s\nfile: %s\n' "${label}" "${needle}" "${file_path}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_contains() {
  local needle="$1"
  local file_path="$2"
  local label="$3"

  if ! grep -q -- "${needle}" "${file_path}"; then
    printf 'FAIL: %s\nmissing pattern: %s\nfile: %s\n' "${label}" "${needle}" "${file_path}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_not_contains '^  create-release:' "${WORKFLOW}" "release workflow has no duplicate GitHub Release job"
assert_not_contains 'gh release create' "${WORKFLOW}" "release workflow does not shell out to gh release create"
assert_not_contains 'docs/CHANGELOG.md' "${WORKFLOW}" "release workflow does not parse changelog notes"

assert_contains 'commands: release' "${WORKFLOW}" "release workflow delegates to homeboy-action release command"
assert_not_contains '--no-github-release' "${RUN_RELEASE}" "run-release lets Homeboy core create GitHub Releases"

printf 'All release workflow checks passed.\n'
