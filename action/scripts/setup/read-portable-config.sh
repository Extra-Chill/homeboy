#!/usr/bin/env bash

# Read homeboy.json and infer configuration that consumers no longer need to specify.
#
# Infers:
#   PORTABLE_ID         — component id
#   PORTABLE_EXTENSION  — extension id (first key from extensions object)
#   PORTABLE_PHP        — php version (from extensions.<ext>.php or composer.json)
#   PORTABLE_NODE       — node version (from extensions.<ext>.node or package.json)
#
# All values are written to GITHUB_ENV and GITHUB_OUTPUT for use by subsequent steps.

set -euo pipefail

if [ ! -f "homeboy.json" ]; then
  echo "::error::homeboy.json is required at repository root"
  echo "::error::Create homeboy.json with at least an \"id\" field and re-run CI"
  exit 1
fi

# ── Component ID ──

PORTABLE_ID="$(jq -r '.id // empty' homeboy.json 2>/dev/null || true)"
if [ -z "${PORTABLE_ID}" ]; then
  echo "::error::homeboy.json must include a top-level \"id\" field"
  exit 1
fi

# ── Extension inference ──
# If the action input specifies an extension, use that. Otherwise infer from
# the first (usually only) key in the extensions object.

EXTENSION_INPUT="${EXTENSION_INPUT:-}"
if [ -n "${EXTENSION_INPUT}" ]; then
  PORTABLE_EXTENSION="${EXTENSION_INPUT}"
else
  PORTABLE_EXTENSION="$(jq -r '.extensions // {} | keys | first // empty' homeboy.json 2>/dev/null || true)"
fi

# ── PHP version inference ──
# Priority: action input > extensions.<ext>.php > composer.json require.php > default
# Default is only set when extension is wordpress (PHP extensions need PHP).

PHP_INPUT="${PHP_INPUT:-}"
PORTABLE_PHP=""

if [ -n "${PHP_INPUT}" ]; then
  PORTABLE_PHP="${PHP_INPUT}"
elif [ -n "${PORTABLE_EXTENSION}" ]; then
  # Check extensions.<ext>.php in homeboy.json
  PORTABLE_PHP="$(jq -r --arg ext "${PORTABLE_EXTENSION}" '.extensions[$ext].php // empty' homeboy.json 2>/dev/null || true)"
fi

if [ -z "${PORTABLE_PHP}" ] && [ -f "composer.json" ]; then
  # Parse minimum PHP version from composer.json require.php constraint
  # Handles: ">=8.2", "^8.2", "~8.2", "8.2.*", ">=8.2 <8.4", "8.2"
  RAW_PHP="$(jq -r '.require.php // empty' composer.json 2>/dev/null || true)"
  if [ -n "${RAW_PHP}" ]; then
    PORTABLE_PHP="$(printf '%s' "${RAW_PHP}" | grep -oP '\d+\.\d+' | head -1 || true)"
  fi
fi

# Default for WordPress extension when nothing else resolved
if [ -z "${PORTABLE_PHP}" ] && [ "${PORTABLE_EXTENSION}" = "wordpress" ]; then
  PORTABLE_PHP="8.2"
fi

# ── Node version inference ──
# Priority: action input > extensions.<ext>.node > package.json engines.node > skip

NODE_INPUT="${NODE_INPUT:-}"
PORTABLE_NODE=""

if [ -n "${NODE_INPUT}" ]; then
  PORTABLE_NODE="${NODE_INPUT}"
elif [ -n "${PORTABLE_EXTENSION}" ]; then
  PORTABLE_NODE="$(jq -r --arg ext "${PORTABLE_EXTENSION}" '.extensions[$ext].node // empty' homeboy.json 2>/dev/null || true)"
fi

if [ -z "${PORTABLE_NODE}" ] && [ -f "package.json" ]; then
  RAW_NODE="$(jq -r '.engines.node // empty' package.json 2>/dev/null || true)"
  if [ -n "${RAW_NODE}" ]; then
    PORTABLE_NODE="$(printf '%s' "${RAW_NODE}" | grep -oP '\d+' | head -1 || true)"
  fi
fi

# ── Write outputs ──

echo "PORTABLE_ID=${PORTABLE_ID}" >> "${GITHUB_ENV}"
echo "PORTABLE_EXTENSION=${PORTABLE_EXTENSION}" >> "${GITHUB_ENV}"
echo "PORTABLE_PHP=${PORTABLE_PHP}" >> "${GITHUB_ENV}"
echo "PORTABLE_NODE=${PORTABLE_NODE}" >> "${GITHUB_ENV}"

echo "portable-id=${PORTABLE_ID}" >> "${GITHUB_OUTPUT}"
echo "portable-extension=${PORTABLE_EXTENSION}" >> "${GITHUB_OUTPUT}"
echo "portable-php=${PORTABLE_PHP}" >> "${GITHUB_OUTPUT}"
echo "portable-node=${PORTABLE_NODE}" >> "${GITHUB_OUTPUT}"

echo "Config resolved from homeboy.json:"
echo "  id:        ${PORTABLE_ID}"
echo "  extension: ${PORTABLE_EXTENSION:-none}"
echo "  php:       ${PORTABLE_PHP:-skip}"
echo "  node:      ${PORTABLE_NODE:-skip}"
