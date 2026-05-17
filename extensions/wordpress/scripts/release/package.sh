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
#                            release.tag, and release.component_id. Not parsed
#                            here because build.sh derives names from
#                            HOMEBOY_COMPONENT_ID and plugin/theme headers
#                            directly. The publish step consumes the tag.

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

jq -cn --arg path "${ARTIFACT_PATH}" \
  '[{path: $path, type: "wordpress-zip", platform: null}]'
