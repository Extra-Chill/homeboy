#!/usr/bin/env bash

set -euo pipefail

HOMEBOY_CLI_VERSION="$(homeboy --version 2>/dev/null || echo 'unknown')"

if [ -n "${EXTENSION_ID:-}" ]; then
  EXTENSION_ID_EFFECTIVE="${EXTENSION_ID}"
else
  EXTENSION_ID_EFFECTIVE="auto"
fi

EXTENSION_SOURCE_EFFECTIVE="${EXTENSION_SOURCE:-auto}"
EXTENSION_REVISION="unknown"

if [ -n "${EXTENSION_ID:-}" ]; then
  EXT_DIR="${HOME}/.config/homeboy/extensions/${EXTENSION_ID}"
  if [ -d "${EXT_DIR}/.git" ]; then
    EXTENSION_REVISION="$(git -C "${EXT_DIR}" rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
  fi
fi

ACTION_REF_USED="${ACTION_REF:-unknown}"
ACTION_REPOSITORY_USED="${ACTION_REPOSITORY:-unknown}"

echo "HOMEBOY_CLI_VERSION=${HOMEBOY_CLI_VERSION}" >> "${GITHUB_ENV}"
echo "HOMEBOY_EXTENSION_ID=${EXTENSION_ID_EFFECTIVE}" >> "${GITHUB_ENV}"
echo "HOMEBOY_EXTENSION_SOURCE=${EXTENSION_SOURCE_EFFECTIVE}" >> "${GITHUB_ENV}"
echo "HOMEBOY_EXTENSION_REVISION=${EXTENSION_REVISION}" >> "${GITHUB_ENV}"
echo "HOMEBOY_ACTION_REF=${ACTION_REF_USED}" >> "${GITHUB_ENV}"
echo "HOMEBOY_ACTION_REPOSITORY=${ACTION_REPOSITORY_USED}" >> "${GITHUB_ENV}"

echo "Tooling metadata captured"
echo "- Homeboy CLI: ${HOMEBOY_CLI_VERSION}"
echo "- Extension: ${EXTENSION_ID_EFFECTIVE} (${EXTENSION_SOURCE_EFFECTIVE})"
echo "- Extension revision: ${EXTENSION_REVISION}"
echo "- Action ref: ${ACTION_REF_USED}"
