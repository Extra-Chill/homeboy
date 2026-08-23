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
#   release_provenance_command
#                          - component-configured command run after the ZIP is
#                            staged. It writes the JSON sidecar named by
#                            HOMEBOY_WORDPRESS_RELEASE_SIDECAR_PATH.

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
PACKAGE_ROOT="$(pwd -P)"

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

release_provenance_command="$(printf '%s' "${SETTINGS_JSON}" | jq -r '.release_provenance_command // empty')"
if [[ -n "${release_provenance_command}" ]] && [[ "$(printf '%s' "${SETTINGS_JSON}" | jq -r '.release_provenance_command | type')" != "string" ]]; then
  echo "Error: release_provenance_command must be a string" >&2
  exit 1
fi

assert_contained_regular_file() {
  local path="$1"
  local label="$2"
  local resolved_path=""

  if [[ -L "${path}" ]] || [[ ! -f "${path}" ]]; then
    echo "Error: ${label} must be a regular non-symlink file: ${path}" >&2
    exit 1
  fi
  resolved_path="$(cd -P "$(dirname "${path}")" && pwd)/$(basename "${path}")"
  case "${resolved_path}" in
    "${PACKAGE_ROOT}"/*) ;;
    *) echo "Error: ${label} must remain within package root: ${path}" >&2; exit 1;;
  esac
}

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
SIDECAR_PATH="build/${COMPONENT_SLUG}.provenance.json"

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

if [[ -n "${release_provenance_command}" ]]; then
  SOURCE_VERSION="$(printf '%s' "${SETTINGS_JSON}" | jq -r '.release.version // empty')"
  SOURCE_TAG="$(printf '%s' "${SETTINGS_JSON}" | jq -r '.release.tag // empty')"
  SOURCE_COMMIT="$(printf '%s' "${SETTINGS_JSON}" | jq -r '.release.commit // .release.source_commit // .release.sha // empty')"
  if [[ -z "${SOURCE_VERSION}" || -z "${SOURCE_TAG}" || -z "${SOURCE_COMMIT}" ]]; then
    echo "Error: release_provenance_command requires release.version, release.tag, and release.commit" >&2
    exit 1
  fi

  artifact_absolute_path="$(cd "$(dirname "${ARTIFACT_PATH}")" && pwd -P)/$(basename "${ARTIFACT_PATH}")"
  sidecar_absolute_path="$(cd "$(dirname "${SIDECAR_PATH}")" && pwd -P)/$(basename "${SIDECAR_PATH}")"
  assert_contained_regular_file "${artifact_absolute_path}" "release ZIP"
  case "$(dirname "${sidecar_absolute_path}")" in "${PACKAGE_ROOT}"/*) ;; *) echo "Error: release provenance sidecar must remain within package root" >&2; exit 1;; esac

  export HOMEBOY_WORDPRESS_RELEASE_PACKAGE_ROOT="${PACKAGE_ROOT}"
  export HOMEBOY_WORDPRESS_RELEASE_SOURCE_VERSION="${SOURCE_VERSION}"
  export HOMEBOY_WORDPRESS_RELEASE_SOURCE_TAG="${SOURCE_TAG}"
  export HOMEBOY_WORDPRESS_RELEASE_SOURCE_COMMIT="${SOURCE_COMMIT}"
  export HOMEBOY_WORDPRESS_RELEASE_ZIP_PATH="${artifact_absolute_path}"
  export HOMEBOY_WORDPRESS_RELEASE_OUTPUT_PATH="$(dirname "${artifact_absolute_path}")"
  export HOMEBOY_WORDPRESS_RELEASE_SIDECAR_PATH="${sidecar_absolute_path}"

  rm -f "${sidecar_absolute_path}"
  if ! bash -c "${release_provenance_command}"; then
    echo "Error: release_provenance_command failed" >&2
    exit 1
  fi
  assert_contained_regular_file "${artifact_absolute_path}" "release ZIP"
  assert_contained_regular_file "${sidecar_absolute_path}" "release provenance sidecar"
  if ! jq -e 'type == "object"' "${sidecar_absolute_path}" >/dev/null; then
    echo "Error: release_provenance_command must write a JSON object sidecar at ${SIDECAR_PATH}" >&2
    exit 1
  fi

  ZIP_SHA256="$(shasum -a 256 "${artifact_absolute_path}")"
  ZIP_SHA256="${ZIP_SHA256%% *}"
  sidecar_tmp="${sidecar_absolute_path}.tmp"
  jq \
    --arg version "${SOURCE_VERSION}" \
    --arg tag "${SOURCE_TAG}" \
    --arg commit "${SOURCE_COMMIT}" \
    --arg zip_path "${ARTIFACT_PATH}" \
    --arg zip_sha256 "${ZIP_SHA256}" \
    '. + {homeboy_wordpress_release_provenance: {version: 1, source: {version: $version, tag: $tag, commit: $commit}, zip: {path: $zip_path, sha256: $zip_sha256}}}' \
    "${sidecar_absolute_path}" > "${sidecar_tmp}"
  mv "${sidecar_tmp}" "${sidecar_absolute_path}"
  echo "Prepared and sealed ${SIDECAR_PATH}" >&2

  jq -cn --arg path "${ARTIFACT_PATH}" --arg sidecar "${SIDECAR_PATH}" \
    '[{path: $path, type: "wordpress-zip", platform: null}, {path: $sidecar, type: "wordpress-provenance", platform: null}]'
else
  jq -cn --arg path "${ARTIFACT_PATH}" \
    '[{path: $path, type: "wordpress-zip", platform: null}]'
fi
