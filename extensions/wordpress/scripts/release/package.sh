#!/usr/bin/env bash
set -euo pipefail

# Build a vendor-bundled WordPress plugin or theme ZIP and emit machine-readable
# artifact JSON for the release pipeline. Human-readable logs go to stderr;
# only the final JSON payload is written to stdout.
#
# Reuses scripts/build/build.sh (the same script the build command runs) so
# release artifacts are produced exactly the same way as developer builds.
# That keeps a single source of truth for build behaviour and avoids
# release-only surprises like missing vendor directories or stripped files.
#
# Output JSON shape (matches the homeboy release pipeline contract):
#   [{"path": "build/<component>.zip", "type": "wordpress-zip", "platform": null}]
#
# Env vars expected by the upstream pipeline:
#   HOMEBOY_COMPONENT_ID   - component id (used by build.sh to name the ZIP)
#   HOMEBOY_RUNTIME_*      - resolve-context helpers (set automatically)
#
# Optional:
#   HOMEBOY_SETTINGS_JSON  - JSON payload from homeboy with release.version,
#                            release.tag, release.component_id, and any dynamic
#                            settings. Invalid or missing JSON is treated as {}.
#   HOMEBOY_COMPONENT_PATH - original component checkout path. Preferred over
#                            release.local_path because release package preflight
#                            may run from a temporary package context.
#   HOMEBOY_WORDPRESS_PACKAGE_SOURCE_PATH
#                          - explicit source checkout override for package builds.
#   HOMEBOY_RELEASE_SOURCE_PATH
#                          - generic release source checkout path from Homeboy.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
BUILD_SCRIPT="${EXTENSION_PATH}/scripts/build/build.sh"

if [[ ! -x "${BUILD_SCRIPT}" ]] && [[ ! -r "${BUILD_SCRIPT}" ]]; then
  echo "Error: build script not found at ${BUILD_SCRIPT}" >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "Error: jq is required to emit release artifact JSON" >&2
  exit 1
fi

RELEASE_LOCAL_PATH="$(printf '%s' "${HOMEBOY_SETTINGS_JSON:-}" | jq -r 'try (.release.local_path // empty) catch empty' 2>/dev/null || true)"
RELEASE_SOURCE_PATH="$(printf '%s' "${HOMEBOY_SETTINGS_JSON:-}" | jq -r 'try (.release.source_path // empty) catch empty' 2>/dev/null || true)"
PACKAGE_SOURCE_PATH="${HOMEBOY_WORDPRESS_PACKAGE_SOURCE_PATH:-${HOMEBOY_RELEASE_SOURCE_PATH:-${RELEASE_SOURCE_PATH:-${HOMEBOY_COMPONENT_PATH:-${RELEASE_LOCAL_PATH}}}}}"
if [[ -n "${PACKAGE_SOURCE_PATH}" ]]; then
  if [[ ! -d "${PACKAGE_SOURCE_PATH}" ]]; then
    echo "Error: WordPress package source path is not a directory: ${PACKAGE_SOURCE_PATH}" >&2
    exit 1
  fi
  cd "${PACKAGE_SOURCE_PATH}"
fi
export HOMEBOY_COMPONENT_PATH="$(pwd)"

COMPONENT_SETTINGS_JSON="{}"
if [[ -f homeboy.json ]]; then
  COMPONENT_SETTINGS_JSON="$(jq -c '.extensions.wordpress.settings // {} | if type == "object" then . else {} end' homeboy.json 2>/dev/null || printf '{}')"
fi

SETTINGS_JSON="$(jq -cn \
  --argjson component "${COMPONENT_SETTINGS_JSON}" \
  --arg payload "${HOMEBOY_SETTINGS_JSON:-}" \
  '
    def object_or_empty($value):
      try ($value | fromjson | if type == "object" then . else {} end) catch {};

    $component + object_or_empty($payload)
  ')"
export HOMEBOY_SETTINGS_JSON="${SETTINGS_JSON}"

# Resolve the component slug the way build.sh does (so the ZIP path matches).
COMPONENT_SLUG="${HOMEBOY_COMPONENT_ID:-}"
if [[ -z "${COMPONENT_SLUG}" ]]; then
  # Fall back to a plugin/theme header probe so this script also works when
  # invoked outside the homeboy release pipeline (developer dry-runs).
  for candidate in *.php; do
    [[ -f "${candidate}" ]] || continue
    if grep -q "^[[:space:]]*\*\?[[:space:]]*Plugin Name:" "${candidate}"; then
      COMPONENT_SLUG="${candidate%.php}"
      break
    fi
  done
  if [[ -z "${COMPONENT_SLUG}" ]] && [[ -f "style.css" ]]; then
    if grep -q "^[[:space:]]*\*\?[[:space:]]*Theme Name:" "style.css"; then
      COMPONENT_SLUG="$(basename "$(pwd)")"
    fi
  fi
fi

if [[ -z "${COMPONENT_SLUG}" ]]; then
  echo "Error: could not determine WordPress component slug (set HOMEBOY_COMPONENT_ID)" >&2
  exit 1
fi

echo "Building release ZIP for ${COMPONENT_SLUG}..." >&2

# Run the standard build pipeline. build.sh produces build/${COMPONENT_SLUG}.zip
# and prints its own progress to stdout/stderr; redirect stdout to stderr so we
# can keep the artifact JSON line clean.
bash "${BUILD_SCRIPT}" >&2

ARTIFACT_PATH="build/${COMPONENT_SLUG}.zip"

if [[ ! -f "${ARTIFACT_PATH}" ]]; then
  echo "Error: expected ZIP at ${ARTIFACT_PATH} but none was produced" >&2
  exit 1
fi

echo "Built ${ARTIFACT_PATH}" >&2

# Assert the ZIP we just built actually contains the version this release
# is shipping. Catches stale artifacts (e.g. a pre-existing build/*.zip that
# was restored instead of rebuilt) before they can reach the publish step.
# The expected version comes from the release payload when homeboy invokes
# this action; standalone dry-runs fall back to the on-disk header so the
# check still validates build output against source.
EXPECTED_VERSION=""
EXPECTED_VERSION="$(echo "${HOMEBOY_SETTINGS_JSON}" | jq -r '.release.version // empty')"
if [[ -z "${EXPECTED_VERSION}" ]]; then
  for candidate in *.php; do
    [[ -f "${candidate}" ]] || continue
    if grep -qi 'Plugin Name:' "${candidate}"; then
      EXPECTED_VERSION="$(grep -i -m1 'Version:' "${candidate}" | sed 's/.*[Vv]ersion:[[:space:]]*//' | tr -d '[:space:]')"
      break
    fi
  done
  if [[ -z "${EXPECTED_VERSION}" ]] && [[ -f "style.css" ]]; then
    EXPECTED_VERSION="$(grep -i -m1 'Version:' "style.css" | sed 's/.*[Vv]ersion:[[:space:]]*//' | tr -d '[:space:]')"
  fi
fi

if [[ -n "${EXPECTED_VERSION}" ]]; then
  bash "${SCRIPT_DIR}/verify-artifact-version.sh" "${ARTIFACT_PATH}" "${EXPECTED_VERSION}" >/dev/null
  echo "Verified ${ARTIFACT_PATH} contains version ${EXPECTED_VERSION}" >&2
else
  echo "Warning: could not determine expected version; skipping artifact version verification" >&2
fi

jq -cn --arg path "${ARTIFACT_PATH}" \
  '[{path: $path, type: "wordpress-zip", platform: null}]'
