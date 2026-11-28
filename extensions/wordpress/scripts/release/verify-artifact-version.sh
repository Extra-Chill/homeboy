#!/usr/bin/env bash
set -euo pipefail

# Refuse to publish a ZIP whose internal version differs from the release.
# Usage: verify-artifact-version.sh <artifact.zip> <expected-version> [entrypoint]
# When the source is available, pass its selected root PHP file or style.css.
# Without source, prefer the wrapper-named plugin, then a unique versioned
# root plugin, or the root style.css for a theme.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/package-entrypoint.sh
source "${SCRIPT_DIR}/../lib/package-entrypoint.sh"

ARTIFACT_PATH="${1:-}"
EXPECTED_VERSION="${2:-}"
MAIN_FILE="${3:-}"

if [[ -z "${ARTIFACT_PATH}" ]] || [[ -z "${EXPECTED_VERSION}" ]]; then
  echo "Usage: verify-artifact-version.sh <artifact.zip> <expected-version> [entrypoint]" >&2
  exit 1
fi

if [[ ! -f "${ARTIFACT_PATH}" ]]; then
  echo "Error: artifact '${ARTIFACT_PATH}' does not exist" >&2
  exit 1
fi

if ! command -v unzip >/dev/null 2>&1; then
  echo "Error: unzip is required to verify artifact versions" >&2
  exit 1
fi

artifact_header() {
  local entry="$1"
  # unzip treats member names as patterns. Escape metacharacters so only
  # the member we selected from the listing can supply the header.
  entry="${entry//\\/\\\\}"
  entry="${entry//\[/\\[}"
  entry="${entry//\*/\\*}"
  entry="${entry//\?/\\?}"
  unzip -p "${ARTIFACT_PATH}" "${entry}" | { head -c 8192; cat >/dev/null; }
}

ARTIFACT_VERSION=""
selected=false
plugin_count=0
versioned_count=0
plugin_version=""
theme_version=""
entries="$(unzip -Z1 "${ARTIFACT_PATH}")"
while IFS= read -r entry; do
  [[ "${entry}" =~ ^[^/]+/[^/]+$ ]] || continue
  filename="${entry#*/}"
  [[ -z "${MAIN_FILE}" || "${filename}" == "${MAIN_FILE}" ]] || continue
  case "${filename}" in
    *.php) field='Plugin Name' ;;
    style.css) field='Theme Name' ;;
    *) continue ;;
  esac
  header="$(artifact_header "${entry}")"
  [[ -n "$(wordpress_package_header "${field}" <<< "${header}")" ]] || continue
  version="$(wordpress_package_header Version <<< "${header}")"

  if [[ -n "${MAIN_FILE}" || "${filename}" == "${entry%%/*}.php" ]]; then
    ARTIFACT_VERSION="${version}"
    selected=true
    break
  elif [[ "${filename}" == style.css ]]; then
    theme_version="${version}"
  else
    plugin_count=$((plugin_count + 1))
    if [[ -n "${version}" ]]; then
      versioned_count=$((versioned_count + 1))
      plugin_version="${version}"
    fi
  fi
done <<< "${entries}"

if [[ -z "${MAIN_FILE}" && "${selected}" == false ]]; then
  if [[ "${versioned_count}" -eq 1 ]]; then
    ARTIFACT_VERSION="${plugin_version}"
  elif [[ "${versioned_count}" -gt 1 ]]; then
    echo "Error: ambiguous WordPress plugin entrypoint inside ${ARTIFACT_PATH}" >&2
    exit 1
  elif [[ "${plugin_count}" -eq 0 ]]; then
    ARTIFACT_VERSION="${theme_version}"
  fi
fi

if [[ -z "${ARTIFACT_VERSION}" ]]; then
  echo "Error: could not locate a plugin/theme version header inside ${ARTIFACT_PATH}${MAIN_FILE:+ for ${MAIN_FILE}}" >&2
  exit 1
fi

if [[ "${ARTIFACT_VERSION}" != "${EXPECTED_VERSION}" ]]; then
  echo "Error: artifact ${ARTIFACT_PATH} contains version ${ARTIFACT_VERSION} but the release expects ${EXPECTED_VERSION} — refusing to ship a stale artifact" >&2
  exit 1
fi

echo "${ARTIFACT_VERSION}"
