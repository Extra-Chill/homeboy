#!/usr/bin/env bash
# Publish one homeboy-action release from this repository's action/ directory
# to the Extra-Chill/homeboy-action repository, where callers resolve it.
#
# The action's history was imported here with rewritten SHAs, so the published
# repository cannot mirror this history without orphaning every SHA callers
# pinned. Each release is appended instead: one commit on top of the published
# main whose tree is exactly action/ at the release tag. Nothing is
# force-pushed, so every existing tag and pinned SHA stays valid.
#
# Usage: publish-action-mirror.sh <version>   (e.g. 2.26.0)
#
# Env:
#   MIRROR_URL    push/fetch URL of the published repository (required)
#   SOURCE_TAG    tag in this repository (default homeboy-action-v<version>)
#   DRY_RUN       true to stop after building the commit
#   GITHUB_OUTPUT optional; receives commit=, tag=, created=
#
# Idempotent: when v<version> already exists on the mirror with the same tree,
# it reports that commit and exits 0. A tag with a different tree is an error.
set -euo pipefail

version="${1:?usage: publish-action-mirror.sh <version>}"
: "${MIRROR_URL:?MIRROR_URL is required}"
source_tag="${SOURCE_TAG:-homeboy-action-v${version}}"
mirror_tag="v${version}"
dry_run="${DRY_RUN:-false}"

emit() { if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "$1" >> "${GITHUB_OUTPUT}"; fi; }

tree="$(git rev-parse --verify "${source_tag}^{commit}:action")"
source_sha="$(git rev-parse --verify "${source_tag}^{commit}")"

git fetch --quiet --no-tags "${MIRROR_URL}" "+refs/heads/main:refs/action-mirror/main"
parent="$(git rev-parse refs/action-mirror/main)"

existing="$(git ls-remote --tags "${MIRROR_URL}" "refs/tags/${mirror_tag}" | awk '{print $1}')"
if [ -n "${existing}" ]; then
  git fetch --quiet --no-tags "${MIRROR_URL}" "+refs/tags/${mirror_tag}:refs/action-mirror/tag"
  existing_tree="$(git rev-parse "refs/action-mirror/tag^{tree}")"
  existing_commit="$(git rev-parse "refs/action-mirror/tag^{commit}")"
  if [ "${existing_tree}" != "${tree}" ]; then
    echo "::error::${mirror_tag} already exists on the mirror with a different tree (${existing_tree}, expected ${tree})." >&2
    exit 1
  fi
  echo "${mirror_tag} is already published as ${existing_commit}."
  emit "commit=${existing_commit}"; emit "tag=${mirror_tag}"; emit "created=false"
  exit 0
fi

message="release: ${mirror_tag}

Published from Extra-Chill/homeboy@${source_sha} (${source_tag}).
Develop in https://github.com/Extra-Chill/homeboy/tree/main/action."

commit="$(GIT_AUTHOR_NAME="${GIT_AUTHOR_NAME:-homeboy-ci[bot]}" \
  GIT_AUTHOR_EMAIL="${GIT_AUTHOR_EMAIL:-homeboy-ci[bot]@users.noreply.github.com}" \
  GIT_COMMITTER_NAME="${GIT_COMMITTER_NAME:-homeboy-ci[bot]}" \
  GIT_COMMITTER_EMAIL="${GIT_COMMITTER_EMAIL:-homeboy-ci[bot]@users.noreply.github.com}" \
  git commit-tree "${tree}" -p "${parent}" -m "${message}")"

echo "Built ${commit} (tree ${tree}) on top of mirror main ${parent}."
if [ "${dry_run}" = "true" ]; then
  echo "DRY_RUN: not pushing."
  emit "commit=${commit}"; emit "tag=${mirror_tag}"; emit "created=false"
  exit 0
fi

# Fast-forward only: a concurrent push to the mirror makes this fail loudly
# instead of overwriting it.
git push --quiet "${MIRROR_URL}" "${commit}:refs/heads/main"
git push --quiet "${MIRROR_URL}" "${commit}:refs/tags/${mirror_tag}"
echo "Published ${mirror_tag} as ${commit}."
emit "commit=${commit}"; emit "tag=${mirror_tag}"; emit "created=true"
