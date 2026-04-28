#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/release.yml"
RUN_RELEASE="${ROOT_DIR}/scripts/release/run-release.sh"
README="${ROOT_DIR}/README.md"
COMMENT_SECTIONS="${ROOT_DIR}/scripts/pr/comment/sections.sh"
VERSION_FILE="${ROOT_DIR}/VERSION"

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
assert_contains 'homeboy_verify_github_release_exists "${ACTUAL_TAG}" "${GITHUB_REPOSITORY:-}"' "${RUN_RELEASE}" "run-release verifies the GitHub Release after successful release"
assert_contains 'release-verify-github-release:' "${ROOT_DIR}/action.yml" "action exposes GitHub Release verification toggle"
assert_contains 'HOMEBOY_VERIFY_GITHUB_RELEASE: ${{ inputs.release-verify-github-release }}' "${ROOT_DIR}/action.yml" "action passes verification toggle to release script"
assert_contains '^2\.' "${VERSION_FILE}" "VERSION is aligned with the v2 action channel"
assert_not_contains 'Extra-Chill/homeboy-action@v1' "${README}" "README examples use the v2 action channel"
assert_contains 'Extra-Chill/homeboy-action@v2' "${README}" "README documents the v2 action channel"
assert_not_contains 'Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v1' "${COMMENT_SECTIONS}" "PR comment footer does not advertise v1"
assert_not_contains 'Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v2' "${COMMENT_SECTIONS}" "PR comment footer does not duplicate action metadata"

printf 'All release workflow checks passed.\n'
