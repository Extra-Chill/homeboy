#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PREPARE="${ROOT}/scripts/core/prepare-test-shard-workspace.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

for phase in candidate baseline; do
  workspace="${tmp}/${phase}"
  mkdir -p "${workspace}"
  git -C "${workspace}" init -q
  git -C "${workspace}" config user.name fixture
  git -C "${workspace}" config user.email fixture@example.invalid
  printf 'consumer\n' > "${workspace}/consumer.txt"
  git -C "${workspace}" add consumer.txt
  git -C "${workspace}" commit -qm fixture
  git -C "${workspace}" init -q .homeboy-action

  GITHUB_WORKSPACE="${workspace}" bash "${PREPARE}"
  mkdir -p "${workspace}/.homeboy-action"
  : > "${workspace}/homeboy-test-inventory.json"
  : > "${workspace}/homeboy-test-shard-plan.json"
  : > "${workspace}/homeboy-test-shard.json"

  status="$(git -C "${workspace}" status --porcelain)"
  [ -z "${status}" ] || { printf 'FAIL: %s replay sees action-owned transport as consumer changes: %s\n' "${phase}" "${status}"; exit 1; }

  printf 'dirty\n' >> "${workspace}/consumer.txt"
  : > "${workspace}/consumer-change.txt"
  status="$(git -C "${workspace}" status --porcelain)"
  printf '%s\n' "${status}" | grep -Fx ' M consumer.txt' >/dev/null || { printf 'FAIL: %s replay hid a real tracked consumer change\n' "${phase}"; exit 1; }
  printf '%s\n' "${status}" | grep -Fx '?? consumer-change.txt' >/dev/null || { printf 'FAIL: %s replay hid a real untracked consumer change\n' "${phase}"; exit 1; }
done

for collision in homeboy-test-inventory.json homeboy-test-shard-plan.json homeboy-test-shard.json; do
  workspace="${tmp}/collision-${collision}"
  mkdir -p "${workspace}"
  git -C "${workspace}" init -q
  git -C "${workspace}" init -q .homeboy-action
  : > "${workspace}/${collision}"
  if GITHUB_WORKSPACE="${workspace}" bash "${PREPARE}" >/dev/null 2>&1; then
    printf 'FAIL: action accepted preexisting consumer transport collision: %s\n' "${collision}"
    exit 1
  fi
done

workspace="${tmp}/action-checkout-collision"
mkdir -p "${workspace}/.homeboy-action"
git -C "${workspace}" init -q
printf 'consumer-owned\n' > "${workspace}/.homeboy-action/marker"
git -C "${workspace}" add .homeboy-action
git -C "${workspace}" config user.name fixture
git -C "${workspace}" config user.email fixture@example.invalid
git -C "${workspace}" commit -qm fixture
if ! git -C "${workspace}" ls-files --error-unmatch -- .homeboy-action >/dev/null 2>&1; then
  printf 'FAIL: fixture did not retain a consumer-owned action checkout collision\n'
  exit 1
fi

printf 'PASS: candidate and baseline Test shard transport stays out of consumer status without hiding real changes\n'
