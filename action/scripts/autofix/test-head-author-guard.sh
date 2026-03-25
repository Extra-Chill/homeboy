#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

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

TMP_REPO="$(mktemp -d)"
trap 'rm -rf "${TMP_REPO}"' EXIT

git -C "${TMP_REPO}" init >/dev/null 2>&1
git -C "${TMP_REPO}" config user.name "Test User"
git -C "${TMP_REPO}" config user.email "test@example.com"

printf 'hello\n' > "${TMP_REPO}/README.md"
git -C "${TMP_REPO}" add README.md
git -C "${TMP_REPO}" commit -m "feat: human commit" >/dev/null 2>&1

pushd "${TMP_REPO}" >/dev/null
source "${ROOT_DIR}/scripts/core/lib.sh"

if head_commit_is_autofix_bot; then
  echo 'FAIL: human HEAD commit should not be treated as bot-authored'
  exit 1
fi
printf 'PASS: human HEAD commit is allowed\n'

GIT_AUTHOR_NAME="${AUTOFIX_BOT_NAME}" \
GIT_AUTHOR_EMAIL="${AUTOFIX_BOT_EMAIL}" \
GIT_COMMITTER_NAME="${AUTOFIX_BOT_NAME}" \
GIT_COMMITTER_EMAIL="${AUTOFIX_BOT_EMAIL}" \
  git commit --allow-empty -m "${AUTOFIX_COMMIT_PREFIX} — refactor (1 files, 1 fixes)" >/dev/null 2>&1

assert_equals "true" "$(head_commit_is_autofix_bot && echo true || echo false)" "bot HEAD commit is blocked"

popd >/dev/null

printf 'All head author guard checks passed.\n'
