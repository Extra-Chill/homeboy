#!/usr/bin/env bash
set -euo pipefail

# Verify that a packaged WordPress plugin/theme ZIP contains the expected
# version before it is published. Guards against stale artifacts reaching
# GitHub Release assets — the data-machine-socials v0.14.0 incident shipped
# a v0.8.1 zip (restored from a stale git-tracked blob during release
# recovery) as the release asset, and production silently ran rolled-back
# code for 6 days because nothing in the pipeline ever opened the zip.
#
# Usage:
#   verify-artifact-version.sh <artifact.zip> <expected-version>
#
# <expected-version> is the bare semver (no leading "v"). The internal
# version is read from the plugin main-file header (any top-level
# "<root>/<name>.php" containing a "Plugin Name:" header) or, for themes,
# from "<root>/style.css".
#
# Exits 0 and prints the matched version on stdout when versions agree.
# Exits 1 with a diagnostic on stderr when they do not, or when no version
# header can be located inside the artifact.

ARTIFACT_PATH="${1:-}"
EXPECTED_VERSION="${2:-}"

if [[ -z "${ARTIFACT_PATH}" ]] || [[ -z "${EXPECTED_VERSION}" ]]; then
  echo "Usage: verify-artifact-version.sh <artifact.zip> <expected-version>" >&2
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

extract_version_header() {
  # Pull "Version: x.y.z" out of header text on stdin.
  grep -i -m1 '^[[:space:]*]*Version:' | sed 's/.*[Vv]ersion:[[:space:]]*//' | tr -d '[:space:]'
}

ARTIFACT_VERSION=""

# Plugin probe: top-level PHP files inside the wrapper directory that carry
# a "Plugin Name:" header.
while IFS= read -r entry; do
  header="$(unzip -p "${ARTIFACT_PATH}" "${entry}" 2>/dev/null | head -n 60 || true)"
  if echo "${header}" | grep -qi 'Plugin Name:'; then
    candidate="$(echo "${header}" | extract_version_header)"
    if [[ -n "${candidate}" ]]; then
      ARTIFACT_VERSION="${candidate}"
      break
    fi
  fi
done < <(unzip -Z1 "${ARTIFACT_PATH}" | grep -E '^[^/]+/[^/]+\.php$' || true)

# Theme fallback: style.css at the wrapper root.
if [[ -z "${ARTIFACT_VERSION}" ]]; then
  style_entry="$(unzip -Z1 "${ARTIFACT_PATH}" | grep -E '^[^/]+/style\.css$' | head -n 1 || true)"
  if [[ -n "${style_entry}" ]]; then
    ARTIFACT_VERSION="$(unzip -p "${ARTIFACT_PATH}" "${style_entry}" 2>/dev/null | head -n 60 | extract_version_header || true)"
  fi
fi

if [[ -z "${ARTIFACT_VERSION}" ]]; then
  echo "Error: could not locate a plugin/theme version header inside ${ARTIFACT_PATH}" >&2
  exit 1
fi

if [[ "${ARTIFACT_VERSION}" != "${EXPECTED_VERSION}" ]]; then
  echo "Error: artifact ${ARTIFACT_PATH} contains version ${ARTIFACT_VERSION} but the release expects ${EXPECTED_VERSION} — refusing to ship a stale artifact" >&2
  exit 1
fi

echo "${ARTIFACT_VERSION}"
