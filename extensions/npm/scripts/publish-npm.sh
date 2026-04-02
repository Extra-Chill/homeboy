#!/usr/bin/env bash
set -euo pipefail

# Publish an npm package to the configured registry.
#
# Reads settings from HOMEBOY_SETTINGS_JSON:
#   - config.access:   "public" or "restricted" (default: "public")
#   - config.registry: npm registry URL (default: "https://registry.npmjs.org")
#
# The version in package.json should already be bumped by homeboy's release
# pipeline before this script runs.

if [[ ! -f "package.json" ]]; then
  echo "No package.json found in $(pwd)" >&2
  exit 1
fi

# Read package info
PACKAGE_NAME=$(node -e "console.log(require('./package.json').name)" 2>/dev/null)
CURRENT_VERSION=$(node -e "console.log(require('./package.json').version)" 2>/dev/null)

if [[ -z "$PACKAGE_NAME" || -z "$CURRENT_VERSION" ]]; then
  echo "Failed to read name/version from package.json" >&2
  exit 1
fi

# Read settings from HOMEBOY_SETTINGS_JSON (if provided by homeboy)
ACCESS="public"
REGISTRY="https://registry.npmjs.org"

if [[ -n "${HOMEBOY_SETTINGS_JSON:-}" ]]; then
  ACCESS=$(echo "$HOMEBOY_SETTINGS_JSON" | jq -r '.config.access // "public"')
  REGISTRY=$(echo "$HOMEBOY_SETTINGS_JSON" | jq -r '.config.registry // "https://registry.npmjs.org"')
fi

# Check if this version is already published
PUBLISHED_VERSION=$(npm view "$PACKAGE_NAME" version --registry "$REGISTRY" 2>/dev/null || echo "")

if [[ "$CURRENT_VERSION" == "$PUBLISHED_VERSION" ]]; then
  echo "Version $CURRENT_VERSION of $PACKAGE_NAME already published, skipping..." >&2
  exit 0
fi

echo "Publishing $PACKAGE_NAME@$CURRENT_VERSION to $REGISTRY (access: $ACCESS)..." >&2

publish_args=(--access "$ACCESS" --registry "$REGISTRY")

npm publish "${publish_args[@]}" >&2

echo "Successfully published $PACKAGE_NAME@$CURRENT_VERSION" >&2
