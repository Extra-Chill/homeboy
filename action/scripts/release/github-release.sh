#!/usr/bin/env bash

set -euo pipefail

homeboy_should_verify_github_release() {
  case "${HOMEBOY_VERIFY_GITHUB_RELEASE:-true}" in
    true|1|yes|on)
      return 0
      ;;
    false|0|no|off)
      return 1
      ;;
    *)
      echo "::error::Invalid HOMEBOY_VERIFY_GITHUB_RELEASE value '${HOMEBOY_VERIFY_GITHUB_RELEASE}'. Expected true or false."
      return 2
      ;;
  esac
}

homeboy_verify_github_release_exists() {
  local release_tag="$1"
  local repository="$2"

  if homeboy_should_verify_github_release; then
    :
  else
    local decision=$?
    if [ "${decision}" -eq 2 ]; then
      return 1
    fi

    echo "::notice::Skipping GitHub Release verification for ${repository}@${release_tag}"
    return 0
  fi

  if [ -z "${release_tag}" ]; then
    echo "::error::Cannot verify GitHub Release: release tag is empty"
    return 1
  fi

  if [ -z "${repository}" ]; then
    echo "::error::Cannot verify GitHub Release for ${release_tag}: GITHUB_REPOSITORY is empty"
    return 1
  fi

  if ! command -v gh >/dev/null 2>&1; then
    echo "::error::Cannot verify GitHub Release ${repository}@${release_tag}: gh CLI is not available"
    return 1
  fi

  if gh release view "${release_tag}" --repo "${repository}" >/dev/null 2>&1; then
    echo "::notice::Verified GitHub Release ${repository}@${release_tag}"
    return 0
  fi

  echo "::error::GitHub Release not found after successful release: repo=${repository} tag=${release_tag}"
  echo "::error::Expected 'gh release view ${release_tag} --repo ${repository}' to succeed."
  return 1
}
