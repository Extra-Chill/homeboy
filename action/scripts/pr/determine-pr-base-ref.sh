#!/usr/bin/env bash

set -euo pipefail

# Fetch enough ancestry for three-dot diff (base...HEAD) to find the merge base.
# GitHub's default checkout is --depth=1 (shallow), which only has the tip commits.
# Three-dot diffs need to walk the graph backwards from both sides until they meet.
#
# Strategy: deepen the shallow clone to include the base commit's ancestry, then
# verify the merge base is reachable. If not, fall back to full unshallow.

echo "Fetching base ancestry for scoped diffs (${BASE_SHA:0:8})..."

# First try: fetch the base commit with enough depth to find the merge base.
# For most PRs, the base is within ~50 commits of HEAD.
git fetch origin "${BASE_SHA}" --depth=50 2>/dev/null || true

# Deepen HEAD's history to match — the shallow clone may only have 1 commit.
git fetch --deepen=50 2>/dev/null || true

# Verify the merge base is now reachable
if ! git merge-base "${BASE_SHA}" HEAD >/dev/null 2>&1; then
  echo "Merge base not found with depth=50, deepening further..."
  git fetch --deepen=200 2>/dev/null || true

  if ! git merge-base "${BASE_SHA}" HEAD >/dev/null 2>&1; then
    echo "Merge base still not found, unshallowing..."
    git fetch --unshallow 2>/dev/null || true
  fi
fi

if git merge-base "${BASE_SHA}" HEAD >/dev/null 2>&1; then
  MERGE_BASE=$(git merge-base "${BASE_SHA}" HEAD)
  echo "Merge base found: ${MERGE_BASE:0:8}"
else
  echo "::warning::Could not find merge base between ${BASE_SHA:0:8} and HEAD — three-dot diffs will fall back to two-dot"
fi

echo "base-ref=${BASE_SHA}" >> "${GITHUB_OUTPUT}"
echo "HOMEBOY_CHANGED_SINCE=${BASE_SHA}" >> "${GITHUB_ENV}"
