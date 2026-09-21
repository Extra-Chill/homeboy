#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

git init --bare -q "${TMP_DIR}/remote.git"
git clone -q "${TMP_DIR}/remote.git" "${TMP_DIR}/work"
git -C "${TMP_DIR}/work" config user.email test@example.com
git -C "${TMP_DIR}/work" config user.name test
git -C "${TMP_DIR}/work" checkout -q -b main
printf 'one\n' > "${TMP_DIR}/work/file"
git -C "${TMP_DIR}/work" add file
git -C "${TMP_DIR}/work" commit -qm initial
git -C "${TMP_DIR}/work" push -q origin HEAD:main
first_sha="$(git -C "${TMP_DIR}/work" rev-parse HEAD)"

GITHUB_OUTPUT="${TMP_DIR}/valid-output" \
PREPARED_REF="main" \
RELEASE_BRANCH="main" \
  bash -c "cd '${TMP_DIR}/work' && bash '${ROOT_DIR}/scripts/release/resolve-prepared-ref.sh'"

grep -Fqx "source-sha=${first_sha}" "${TMP_DIR}/valid-output"
printf 'PASS: prepared branch resolves to current immutable SHA\n'

printf 'two\n' >> "${TMP_DIR}/work/file"
git -C "${TMP_DIR}/work" add file
git -C "${TMP_DIR}/work" commit -qm newer
git -C "${TMP_DIR}/work" push -q origin HEAD:main

if GITHUB_OUTPUT="${TMP_DIR}/stale-output" \
  PREPARED_REF="${first_sha}" \
  RELEASE_BRANCH="main" \
  bash -c "cd '${TMP_DIR}/work' && bash '${ROOT_DIR}/scripts/release/resolve-prepared-ref.sh'"; then
  printf 'FAIL: stale prepared ref was accepted\n'
  exit 1
fi
printf 'PASS: stale prepared ref is rejected\n'
