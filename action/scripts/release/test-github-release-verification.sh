#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HELPER="${ROOT_DIR}/scripts/release/github-release.sh"

source "${HELPER}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

assert_success() {
  local label="$1"
  shift

  if "$@" >"${TMP_DIR}/stdout" 2>"${TMP_DIR}/stderr"; then
    printf 'PASS: %s\n' "${label}"
    return 0
  fi

  printf 'FAIL: %s\n' "${label}"
  printf 'stdout:\n%s\n' "$(cat "${TMP_DIR}/stdout")"
  printf 'stderr:\n%s\n' "$(cat "${TMP_DIR}/stderr")"
  exit 1
}

assert_failure_contains() {
  local expected="$1"
  local label="$2"
  shift 2

  if "$@" >"${TMP_DIR}/stdout" 2>"${TMP_DIR}/stderr"; then
    printf 'FAIL: %s\nexpected failure containing: %s\n' "${label}" "${expected}"
    exit 1
  fi

  if ! grep -q -- "${expected}" "${TMP_DIR}/stdout" "${TMP_DIR}/stderr"; then
    printf 'FAIL: %s\nmissing expected output: %s\n' "${label}" "${expected}"
    printf 'stdout:\n%s\n' "$(cat "${TMP_DIR}/stdout")"
    printf 'stderr:\n%s\n' "$(cat "${TMP_DIR}/stderr")"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

write_fake_gh() {
  local exit_code="$1"
  cat >"${TMP_DIR}/gh" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" > "${TMP_DIR}/gh-args"
exit ${exit_code}
EOF
  chmod +x "${TMP_DIR}/gh"
}

write_fake_gh 0
PATH="${TMP_DIR}:${PATH}" assert_success \
  "verifies existing GitHub Release" \
  homeboy_verify_github_release_exists "v1.2.3" "Extra-Chill/homeboy-action"

if [ "$(cat "${TMP_DIR}/gh-args")" != "release view v1.2.3 --repo Extra-Chill/homeboy-action" ]; then
  printf 'FAIL: gh release view arguments were not passed through correctly\n'
  printf 'args: %s\n' "$(cat "${TMP_DIR}/gh-args")"
  exit 1
fi
printf 'PASS: gh release view receives tag and repo\n'

write_fake_gh 1
PATH="${TMP_DIR}:${PATH}" assert_failure_contains \
  "GitHub Release not found after successful release: repo=Extra-Chill/homeboy-action tag=v9.9.9" \
  "fails clearly when GitHub Release is missing" \
  homeboy_verify_github_release_exists "v9.9.9" "Extra-Chill/homeboy-action"

HOMEBOY_VERIFY_GITHUB_RELEASE=false assert_success \
  "can skip verification for tag-only release consumers" \
  homeboy_verify_github_release_exists "v1.2.3" "Extra-Chill/homeboy-action"

HOMEBOY_VERIFY_GITHUB_RELEASE=maybe assert_failure_contains \
  "Invalid HOMEBOY_VERIFY_GITHUB_RELEASE value" \
  "rejects invalid verification configuration" \
  homeboy_verify_github_release_exists "v1.2.3" "Extra-Chill/homeboy-action"

assert_failure_contains \
  "release tag is empty" \
  "fails clearly when tag is empty" \
  homeboy_verify_github_release_exists "" "Extra-Chill/homeboy-action"

assert_failure_contains \
  "GITHUB_REPOSITORY is empty" \
  "fails clearly when repository is empty" \
  homeboy_verify_github_release_exists "v1.2.3" ""

printf 'All GitHub Release verification checks passed.\n'
