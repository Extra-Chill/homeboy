#!/usr/bin/env bash
# Exercises scripts/publish-action-mirror.sh against a throwaway bare mirror:
# append on top of mirror main, idempotent rerun, conflicting-tree refusal.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
script="${root}/scripts/publish-action-mirror.sh"
work="$(mktemp -d)"
trap 'rm -rf "${work}"; git -C "${root}" tag -d homeboy-action-v0.0.1-test homeboy-action-v0.0.2-test >/dev/null 2>&1 || true' EXIT

export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com

# A mirror whose main holds an unrelated history.
git init -q -b main "${work}/seed"
echo seed > "${work}/seed/README.md"
git -C "${work}/seed" add README.md
git -C "${work}/seed" commit -qm seed
git -C "${work}/seed" tag v0.0.0
git clone -q --bare "${work}/seed" "${work}/mirror.git"
seed_sha="$(git -C "${work}/mirror.git" rev-parse main)"

head="$(git -C "${root}" rev-parse HEAD)"
git -C "${root}" tag -f homeboy-action-v0.0.1-test "${head}" >/dev/null
export MIRROR_URL="${work}/mirror.git"

cd "${root}"
SOURCE_TAG=homeboy-action-v0.0.1-test bash "${script}" 0.0.1-test

published="$(git -C "${work}/mirror.git" rev-parse v0.0.1-test^{commit})"
[ "$(git -C "${work}/mirror.git" rev-parse main)" = "${published}" ] || { echo "FAIL: main not advanced"; exit 1; }
[ "$(git -C "${work}/mirror.git" rev-parse "${published}~1")" = "${seed_sha}" ] || { echo "FAIL: not appended on mirror main"; exit 1; }
[ "$(git -C "${work}/mirror.git" rev-parse "${published}^{tree}")" = "$(git rev-parse "${head}:action")" ] || { echo "FAIL: tree is not action/"; exit 1; }
[ "$(git -C "${work}/mirror.git" rev-parse v0.0.0)" = "${seed_sha}" ] || { echo "FAIL: existing tag moved"; exit 1; }
echo "PASS append"

out="$(SOURCE_TAG=homeboy-action-v0.0.1-test bash "${script}" 0.0.1-test)"
printf '%s\n' "${out}" | grep -q "already published as ${published}" || { echo "FAIL: rerun not idempotent"; exit 1; }
echo "PASS idempotent"

# Same version, different tree: must refuse.
other="$(git rev-list -n 1 "${head}" -- action | head -1)"
other_tree_commit="$(git rev-list -n 1 "${other}~1" -- action 2>/dev/null || true)"
if [ -n "${other_tree_commit}" ] && [ "$(git rev-parse "${other_tree_commit}:action" 2>/dev/null)" != "$(git rev-parse "${head}:action")" ]; then
  git tag -f homeboy-action-v0.0.2-test "${other_tree_commit}" >/dev/null
  if SOURCE_TAG=homeboy-action-v0.0.2-test bash "${script}" 0.0.1-test >/dev/null 2>&1; then
    echo "FAIL: conflicting tree was accepted"; exit 1
  fi
  echo "PASS conflict refused"
else
  echo "SKIP conflict case: no earlier action/ tree in this checkout"
fi
