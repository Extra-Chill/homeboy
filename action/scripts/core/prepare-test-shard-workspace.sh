#!/usr/bin/env bash

# Reserves the action-owned paths used to transport Test shard state. The action
# checkout has already materialized; transport must not make consumer checks
# mistake it for a consumer workspace change.
set -euo pipefail

workspace="${GITHUB_WORKSPACE:-$PWD}"
git -C "${workspace}" rev-parse --is-inside-work-tree >/dev/null || {
  echo "::error::Test shard workspace is not a Git worktree: ${workspace}" >&2
  exit 1
}

for path in homeboy-test-inventory.json homeboy-test-shard-plan.json homeboy-test-shard.json; do
  if git -C "${workspace}" ls-files --error-unmatch -- "${path}" >/dev/null 2>&1 || [ -e "${workspace}/${path}" ] || [ -L "${workspace}/${path}" ]; then
    echo "::error::Refusing to overwrite consumer path reserved for Test shard transport: ${path}" >&2
    exit 1
  fi
done

if [ -L "${workspace}/.homeboy-action" ] || ! git -C "${workspace}/.homeboy-action" rev-parse --is-inside-work-tree >/dev/null; then
  echo "::error::Homeboy Action checkout is missing or unsafe." >&2
  exit 1
fi

exclude_file="$(git -C "${workspace}" rev-parse --path-format=absolute --git-path info/exclude)"
mkdir -p "$(dirname "${exclude_file}")"
printf '%s\n' '/.homeboy-action/' '/homeboy-test-inventory.json' '/homeboy-test-shard-plan.json' '/homeboy-test-shard.json' >> "${exclude_file}"
