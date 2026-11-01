#!/usr/bin/env bash

set -euo pipefail

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

git init -q "${TMP_DIR}/repo"
git -C "${TMP_DIR}/repo" config user.email test@example.com
git -C "${TMP_DIR}/repo" config user.name test
printf 'release\n' > "${TMP_DIR}/repo/file"
git -C "${TMP_DIR}/repo" add file
git -C "${TMP_DIR}/repo" commit -qm release
git -C "${TMP_DIR}/repo" tag v-lightweight
git -C "${TMP_DIR}/repo" tag -a v-annotated -m release

for tag in v-lightweight v-annotated; do
  resolved="$(git -C "${TMP_DIR}/repo" rev-parse --verify "${tag}^{commit}")"
  expected="$(git -C "${TMP_DIR}/repo" rev-parse HEAD)"
  if [ "${resolved}" != "${expected}" ]; then
    printf 'FAIL: %s resolved to %s, expected %s\n' "${tag}" "${resolved}" "${expected}"
    exit 1
  fi
  printf 'PASS: %s resolves to its release commit\n' "${tag}"
done
