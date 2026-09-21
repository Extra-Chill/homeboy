#!/usr/bin/env bash

set -euo pipefail

prepared_ref="${PREPARED_REF:-}"
release_branch="${RELEASE_BRANCH:-main}"
release_branch="${release_branch#refs/heads/}"

remote_ls() {
  if [ -n "${SOURCE_TOKEN:-}" ]; then
    git -c "http.extraheader=AUTHORIZATION: bearer ${SOURCE_TOKEN}" ls-remote origin "$@"
  else
    git ls-remote origin "$@"
  fi
}

if [ -z "${prepared_ref}" ]; then
  echo "source-sha=$(git rev-parse HEAD)" >> "${GITHUB_OUTPUT}"
  echo "source-ref=event" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [[ "${prepared_ref}" =~ ^[0-9a-fA-F]{40}$ ]]; then
  source_sha="${prepared_ref,,}"
else
  refs="$(remote_ls \
    "refs/heads/${prepared_ref}" \
    "refs/tags/${prepared_ref}" \
    "refs/tags/${prepared_ref}^{}")"
  source_sha="$(printf '%s\n' "${refs}" | awk '$2 ~ /refs\/tags\/.*\^\{\}$/ {print $1; exit}')"
  [ -n "${source_sha}" ] || source_sha="$(printf '%s\n' "${refs}" | awk 'NF {print $1; exit}')"
fi

if ! [[ "${source_sha:-}" =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::prepared-ref '${prepared_ref}' did not resolve to a commit SHA."
  exit 1
fi

branch_sha="$(remote_ls "refs/heads/${release_branch}" | awk 'NF {print $1; exit}')"
if [ -z "${branch_sha}" ]; then
  echo "::error::release branch '${release_branch}' could not be resolved on origin."
  exit 1
fi

if [ "${source_sha}" != "${branch_sha}" ]; then
  echo "::error::prepared-ref '${prepared_ref}' resolved to ${source_sha}, but origin/${release_branch} is ${branch_sha}; refusing to release a stale prepared source."
  exit 1
fi

echo "Resolved prepared-ref '${prepared_ref}' to ${source_sha} (current origin/${release_branch})."
echo "source-sha=${source_sha}" >> "${GITHUB_OUTPUT}"
echo "source-ref=${prepared_ref}" >> "${GITHUB_OUTPUT}"
