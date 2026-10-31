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
RELEASE_PACKAGE_SCRIPT=""

if [[ -n "${HOMEBOY_SETTINGS_JSON:-}" ]]; then
  RELEASE_PACKAGE_SCRIPT=$(printf '%s' "${HOMEBOY_SETTINGS_JSON}" | jq -er '.config.release_package_script // "" | strings' 2>/dev/null) || {
    echo "HOMEBOY_SETTINGS_JSON must contain a valid string config.release_package_script" >&2
    exit 1
  }
fi

if [[ -z "${PACKAGE_NAME}" || -z "${PACKAGE_VERSION}" ]]; then
  echo "Failed to read name/version from package.json" >&2
  exit 1
fi

PROJECT_ARTIFACTS='[]'
if [[ -n "${RELEASE_PACKAGE_SCRIPT}" ]]; then
  if ! jq -e --arg script "${RELEASE_PACKAGE_SCRIPT}" '.scripts[$script] | strings | select(length > 0)' package.json >/dev/null; then
    echo "Configured release package script is missing from package.json: ${RELEASE_PACKAGE_SCRIPT}" >&2
    exit 1
  fi

  package_output=$(mktemp)
  trap 'rm -f "${package_output:-}"' EXIT
  echo "Running npm run ${RELEASE_PACKAGE_SCRIPT} for ${PACKAGE_NAME}@${PACKAGE_VERSION}..." >&2
  if ! npm run "${RELEASE_PACKAGE_SCRIPT}" >"${package_output}"; then
    cat "${package_output}" >&2
    echo "Configured release package script failed: ${RELEASE_PACKAGE_SCRIPT}" >&2
    exit 1
  fi
  cat "${package_output}" >&2
  PROJECT_ARTIFACTS=$(node - "${package_output}" <<'NODE'
const fs = require("node:fs")
const output = fs.readFileSync(process.argv[2], "utf8").trim()
if (!output) {
  console.error("Configured release package script produced empty stdout")
  process.exit(1)
}

for (let index = output.length - 1; index >= 0; index--) {
  if (output[index] !== "[" && output[index] !== "{") continue
  try {
    const parsed = JSON.parse(output.slice(index))
    const artifacts = Array.isArray(parsed) ? parsed : [parsed]
    if (artifacts.length === 0 || artifacts.some((artifact) => !artifact || typeof artifact !== "object" || Array.isArray(artifact) || typeof artifact.path !== "string" || artifact.path.length === 0)) continue
    const unique = artifacts.filter((artifact, artifactIndex) => artifacts.findIndex((candidate) => candidate.path === artifact.path) === artifactIndex)
    process.stdout.write(JSON.stringify(unique))
    process.exit(0)
  } catch {}
}

console.error("Configured release package script did not end with a valid artifact JSON object or array")
process.exit(1)
NODE
  ) || exit 1

  while IFS= read -r artifact_path; do
    if [[ ! -f "${artifact_path}" ]]; then
      echo "Configured release package script declared a missing artifact: ${artifact_path}" >&2
      exit 1
    fi
  done < <(printf '%s' "${PROJECT_ARTIFACTS}" | jq -r '.[].path')
else
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

jq -cn --argjson project "${PROJECT_ARTIFACTS}" --arg path "${PACK_TARBALL}" '
  $project + (if any($project[]; .path == $path) then [] else [{path: $path, type: "npm_tarball", platform: null}] end)
'
