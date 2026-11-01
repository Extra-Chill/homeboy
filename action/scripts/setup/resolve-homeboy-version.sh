#!/usr/bin/env bash

set -euo pipefail

REPO="Extra-Chill/homeboy"
ASSET_RETRY_ATTEMPTS="${HOMEBOY_ASSET_RETRY_ATTEMPTS:-3}"
ASSET_RETRY_DELAY="${HOMEBOY_ASSET_RETRY_DELAY:-5}"

# Both endpoints share the stability filter; releases/latest returns one
# release object while the listing returns an array, so the row extraction
# differs.
LATEST_ROWS_JQ='select(.draft == false and .prerelease == false) | . as $release | $release.assets[]? | [$release.tag_name, .name] | @tsv'
LISTING_ROWS_JQ='.[] | select(.draft == false and .prerelease == false) | . as $release | $release.assets[]? | [$release.tag_name, .name] | @tsv'

platform_target() {
  local os arch
  os=$(uname -s | tr '[:upper:]' '[:lower:]')
  arch=$(uname -m)

  case "${os}-${arch}" in
    linux-x86_64) printf 'x86_64-unknown-linux-gnu' ;;
    linux-aarch64) printf 'aarch64-unknown-linux-gnu' ;;
    darwin-x86_64) printf 'x86_64-apple-darwin' ;;
    darwin-arm64) printf 'aarch64-apple-darwin' ;;
    *)
      echo "::error::Unsupported platform: ${os}-${arch}"
      exit 1
      ;;
  esac
}

# stderr of the last gh api call. The releases API intermittently answers
# HTTP 200 with the body `[]`, and without keeping stderr an API failure and
# an empty response are indistinguishable from a missing asset.
GH_STDERR_FILE=""
trap 'rm -f "${GH_STDERR_FILE}"' EXIT
GH_STDERR_FILE="$(mktemp)"

# Runs one gh api call, keeping its stderr in GH_STDERR_FILE for the final
# error message. Prints stdout; returns gh's exit status.
run_gh_api() {
  local query="$1"
  local jq_filter="$2"
  local status=0

  : > "${GH_STDERR_FILE}"
  gh api "${query}" --jq "${jq_filter}" 2>"${GH_STDERR_FILE}" || status=$?
  return "${status}"
}

# Prints one gh api response, retrying failed or empty replies within the
# existing HOMEBOY_ASSET_RETRY_ATTEMPTS/DELAY budget. An empty listing is not
# evidence that an asset is missing; the caller decides from the payload.
api_read() {
  local query="$1"
  local jq_filter="$2"
  local attempt output status

  for attempt in $(seq 1 "${ASSET_RETRY_ATTEMPTS}"); do
    # Neither a failed call nor an empty reply is fatal here: both are
    # transient responses to retry, with gh's stderr already kept for the
    # error message.
    status=0
    output="$(run_gh_api "${query}" "${jq_filter}")" || status=$?
    if [ "${status}" -eq 0 ] && [ -n "${output}" ]; then
      printf '%s\n' "${output}"
      return 0
    fi

    if [ "${attempt}" -lt "${ASSET_RETRY_ATTEMPTS}" ]; then
      echo "gh api ${query} returned nothing; retrying (${attempt}/${ASSET_RETRY_ATTEMPTS})..." >&2
      sleep "${ASSET_RETRY_DELAY}"
    fi
  done

  return 1
}

release_has_asset() {
  local tag="$1"
  local archive="$2"

  run_gh_api "repos/${REPO}/releases/tags/${tag}" '.assets[].name' | grep -Fxq "${archive}"
}

wait_for_asset() {
  local tag="$1"
  local archive="$2"
  local attempt

  for attempt in $(seq 1 "${ASSET_RETRY_ATTEMPTS}"); do
    if release_has_asset "${tag}" "${archive}"; then
      return 0
    fi

    if [ "${attempt}" -lt "${ASSET_RETRY_ATTEMPTS}" ]; then
      echo "Homeboy ${tag#v} asset ${archive} is not available yet; retrying (${attempt}/${ASSET_RETRY_ATTEMPTS})..."
      sleep "${ASSET_RETRY_DELAY}"
    fi
  done

  return 1
}

# Prints the tag of the newest release in the TSV rows that carries the
# archive; empty when none does. Rows arrive newest-first.
newest_tag_with_asset() {
  local rows="$1"
  local archive="$2"
  local tag asset

  while IFS=$'\t' read -r tag asset; do
    if [ "${asset}" = "${archive}" ]; then
      printf '%s' "${tag}"
      return 0
    fi
  done <<< "${rows}"
}

# Prints the tag of the newest stable release carrying the archive, or an
# empty string when neither lookup can confirm one. The small releases/latest
# endpoint is stable by definition, so it is consulted first; the paged scan
# only runs when it does not carry the platform archive. Both lookups retry
# empty or failed responses, so a definitive "no such release" requires a
# non-empty listing without the asset.
resolve_latest_tag() {
  local archive="$1"
  local rows tag=""

  if rows="$(api_read "repos/${REPO}/releases/latest" "${LATEST_ROWS_JQ}")"; then
    tag="$(newest_tag_with_asset "${rows}" "${archive}")"
  fi

  if [ -z "${tag}" ]; then
    if rows="$(api_read "repos/${REPO}/releases?per_page=100" "${LISTING_ROWS_JQ}")"; then
      tag="$(newest_tag_with_asset "${rows}" "${archive}")"
    fi
  fi

  printf '%s' "${tag}"
}

# Appends the last gh stderr, if any, so API failures are visible in the log.
gh_error_detail() {
  local detail
  detail="$(tr '\n' ' ' < "${GH_STDERR_FILE}" | sed 's/ *$//')"
  if [ -n "${detail}" ]; then
    printf '; last gh error: %s' "${detail}"
  fi
}

TARGET="$(platform_target)"
ARCHIVE="homeboy-${TARGET}.tar.xz"

if [ "${HOMEBOY_VERSION}" = "latest" ]; then
  TAG=$(resolve_latest_tag "${ARCHIVE}")
  if [ -z "${TAG}" ]; then
    echo "::error::Could not find a stable Homeboy release with asset ${ARCHIVE}$(gh_error_detail)"
    exit 1
  fi
else
  TAG="v${HOMEBOY_VERSION#v}"
  if ! wait_for_asset "${TAG}" "${ARCHIVE}"; then
    echo "::error::Homeboy ${TAG#v} asset ${ARCHIVE} was not available; explicit versions do not fall back$(gh_error_detail)"
    exit 1
  fi
fi

echo "resolved-version=${TAG#v}" >> "${GITHUB_OUTPUT}"
