#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/release.yml"
RUN_RELEASE="${ROOT_DIR}/scripts/release/run-release.sh"
README="${ROOT_DIR}/README.md"
COMMENT_SECTIONS="${ROOT_DIR}/scripts/pr/comment/sections.sh"
VERSION_FILE="${ROOT_DIR}/VERSION"
HOMEBOY_CONFIG="${ROOT_DIR}/homeboy.json"

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

assert_contains 'uses: ./' "${WORKFLOW}" "release workflow uses checked-out action code for self-release"
assert_not_contains 'Extra-Chill/homeboy-action@v2' "${WORKFLOW}" "release workflow is not bootstrapped from the stale floating v2 channel"
assert_contains 'commands: release' "${WORKFLOW}" "release workflow delegates to homeboy-action release command"
assert_contains 'runner.temp }}/homeboy-release-last-failed' "${WORKFLOW}" "release failure cache lives outside checkout"
assert_not_contains 'path: .release-last-failed' "${WORKFLOW}" "release failure cache is not restored into checkout"
assert_not_contains '< .release-last-failed' "${WORKFLOW}" "release failure marker is not read from checkout"
assert_contains '--no-github-release' "${RUN_RELEASE}" "run-release leaves GitHub Release creation to tag workflow"
assert_contains '--git-identity bot' "${RUN_RELEASE}" "run-release delegates bot identity to Homeboy core"
assert_contains 'GIT_ASKPASS' "${RUN_RELEASE}" "run-release provides token auth to Homeboy git pushes"
assert_not_contains 'git config user.name' "${RUN_RELEASE}" "run-release does not configure git identity directly"
assert_not_contains 'git config --local' "${RUN_RELEASE}" "run-release does not configure git auth directly"
assert_not_contains 'remote set-url origin' "${RUN_RELEASE}" "run-release does not rewrite origin to an authenticated URL"
assert_contains 'homeboy_verify_github_release_exists "${TAG}" "${GITHUB_REPOSITORY:-}"' "${RUN_RELEASE}" "run-release verifies the GitHub Release after successful release"
assert_contains 'release-verify-github-release:' "${ROOT_DIR}/action.yml" "action exposes GitHub Release verification toggle"
assert_contains 'HOMEBOY_VERIFY_GITHUB_RELEASE: ${{ inputs.release-verify-github-release }}' "${ROOT_DIR}/action.yml" "action passes verification toggle to release script"
assert_contains 'release-bump-type:' "${ROOT_DIR}/action.yml" "action exposes release bump type output"
assert_contains 'skipped-reason:' "${ROOT_DIR}/action.yml" "action exposes skipped release reason output"
assert_contains 'git/ref/tags/v${release_version}' "${HOMEBOY_CONFIG}" "post-release hook reads the pushed release tag ref"
assert_contains 'release_sha' "${HOMEBOY_CONFIG}" "post-release hook points v2 at the pushed release tag target"
assert_not_contains 'sha=$(git rev-parse HEAD)' "${HOMEBOY_CONFIG}" "post-release hook does not point v2 at an unpushed local release commit"
assert_not_contains 'Skipping v2 tag update.*||' "${HOMEBOY_CONFIG}" "post-release hook does not swallow failed v2 tag updates"
assert_contains '^2\.' "${VERSION_FILE}" "VERSION is aligned with the v2 action channel"
assert_not_contains 'Extra-Chill/homeboy-action@v1' "${README}" "README examples use the v2 action channel"
assert_contains 'Extra-Chill/homeboy-action@v2' "${README}" "README documents the v2 action channel"
assert_not_contains 'Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v1' "${COMMENT_SECTIONS}" "PR comment footer does not advertise v1"
assert_not_contains 'Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v2' "${COMMENT_SECTIONS}" "PR comment footer does not duplicate action metadata"

printf 'All release workflow checks passed.\n'
