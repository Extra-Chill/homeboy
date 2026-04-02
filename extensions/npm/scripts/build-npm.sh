#!/usr/bin/env bash
set -euo pipefail

# Build the npm package before publishing, then emit machine-readable artifact
# JSON for the release pipeline. The package step MUST print JSON on stdout —
# human logs go to stderr.

if [[ ! -f "package.json" ]]; then
  echo "No package.json found in $(pwd)" >&2
  exit 1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "node is required for npm packaging" >&2
  exit 1
fi

PACKAGE_NAME=$(node -e "console.log(require('./package.json').name || '')" 2>/dev/null)
PACKAGE_VERSION=$(node -e "console.log(require('./package.json').version || '')" 2>/dev/null)

if [[ -z "${PACKAGE_NAME}" || -z "${PACKAGE_VERSION}" ]]; then
  echo "Failed to read name/version from package.json" >&2
  exit 1
fi

# Check if a build script exists.
has_build=$(node -e "
  const pkg = require('./package.json');
  console.log(pkg.scripts && pkg.scripts.build ? 'yes' : 'no');
" 2>/dev/null || echo "no")

if [[ "${has_build}" == "yes" ]]; then
  echo "Running npm run build for ${PACKAGE_NAME}@${PACKAGE_VERSION}..." >&2
  npm run build >&2
else
  echo "No build script defined in package.json, skipping build" >&2
fi

# npm pack --json can emit lifecycle script output (prepack, prepare, etc.) to
# stdout before the JSON array. Capture the full output, then extract only the
# JSON portion so jq doesn't choke on leading non-JSON text.
PACK_OUTPUT=$(npm pack --json)
PACK_JSON=$(echo "${PACK_OUTPUT}" | sed -n '/^\[/,$p')

if [[ -z "${PACK_JSON}" ]]; then
  echo "npm pack --json did not produce valid JSON output" >&2
  echo "Raw output:" >&2
  echo "${PACK_OUTPUT}" >&2
  exit 1
fi

PACK_TARBALL=$(echo "${PACK_JSON}" | jq -r '.[0].filename // empty')

if [[ -z "${PACK_TARBALL}" || ! -f "${PACK_TARBALL}" ]]; then
  echo "npm pack did not produce a tarball" >&2
  exit 1
fi

jq -cn --arg path "${PACK_TARBALL}" '[{path: $path, type: "npm_tarball", platform: null}]'
