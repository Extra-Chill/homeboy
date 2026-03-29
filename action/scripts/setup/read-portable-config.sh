#!/usr/bin/env bash

# Read homeboy.json — the single source of truth for project configuration.
#
# Reads:
#   PORTABLE_ID         — component id
#   PORTABLE_EXTENSION  — extension id (first key from extensions object)
#   PORTABLE_PHP        — php version (from extensions.<ext>.php)
#   PORTABLE_NODE       — node version (from extensions.<ext>.node)
#
# Action inputs can override php and node versions. Everything else
# comes from homeboy.json or it doesn't exist.
#
# All values are written to GITHUB_ENV and GITHUB_OUTPUT for use by subsequent steps.

set -euo pipefail

# ── Resolve config path ──
# When COMPONENT_NAME is set and a homeboy.json exists in that subdirectory,
# read config from there instead of the repo root. This supports multi-component
# repos where each component has its own homeboy.json.

COMPONENT_NAME="${COMPONENT_NAME:-}"
CONFIG_DIR="."

if [ -n "${COMPONENT_NAME}" ] && [ -f "${COMPONENT_NAME}/homeboy.json" ]; then
  CONFIG_DIR="${COMPONENT_NAME}"
  echo "Reading config from component subdirectory: ${CONFIG_DIR}/"
elif [ ! -f "homeboy.json" ]; then
  echo "::error::homeboy.json is required at repository root (or in component subdirectory when component input is set)"
  echo "::error::Create homeboy.json with at least an \"id\" field and re-run CI"
  exit 1
fi

CONFIG_FILE="${CONFIG_DIR}/homeboy.json"

# ── Component ID ──

PORTABLE_ID="$(jq -r '.id // empty' "${CONFIG_FILE}" 2>/dev/null || true)"
if [ -z "${PORTABLE_ID}" ]; then
  echo "::error::${CONFIG_FILE} must include a top-level \"id\" field"
  exit 1
fi

# ── Extension inference ──
# If the action input specifies an extension, use that. Otherwise infer from
# the first (usually only) key in the extensions object.

EXTENSION_INPUT="${EXTENSION_INPUT:-}"
if [ -n "${EXTENSION_INPUT}" ]; then
  PORTABLE_EXTENSION="${EXTENSION_INPUT}"
else
  PORTABLE_EXTENSION="$(jq -r '.extensions // {} | keys | first // empty' "${CONFIG_FILE}" 2>/dev/null || true)"
fi

# ── PHP version ──
# Source: action input override > extensions.<ext>.php in homeboy.json

PHP_INPUT="${PHP_INPUT:-}"
PORTABLE_PHP=""

if [ -n "${PHP_INPUT}" ]; then
  PORTABLE_PHP="${PHP_INPUT}"
elif [ -n "${PORTABLE_EXTENSION}" ]; then
  PORTABLE_PHP="$(jq -r --arg ext "${PORTABLE_EXTENSION}" '.extensions[$ext].php // empty' "${CONFIG_FILE}" 2>/dev/null || true)"
fi

# ── Node version ──
# Source: action input override > extensions.<ext>.node in homeboy.json

NODE_INPUT="${NODE_INPUT:-}"
PORTABLE_NODE=""

if [ -n "${NODE_INPUT}" ]; then
  PORTABLE_NODE="${NODE_INPUT}"
elif [ -n "${PORTABLE_EXTENSION}" ]; then
  PORTABLE_NODE="$(jq -r --arg ext "${PORTABLE_EXTENSION}" '.extensions[$ext].node // empty' "${CONFIG_FILE}" 2>/dev/null || true)"
fi

# ── Write outputs ──

echo "PORTABLE_ID=${PORTABLE_ID}" >> "${GITHUB_ENV}"
echo "PORTABLE_EXTENSION=${PORTABLE_EXTENSION}" >> "${GITHUB_ENV}"
echo "PORTABLE_PHP=${PORTABLE_PHP}" >> "${GITHUB_ENV}"
echo "PORTABLE_NODE=${PORTABLE_NODE}" >> "${GITHUB_ENV}"
echo "COMPONENT_DIR=${CONFIG_DIR}" >> "${GITHUB_ENV}"

echo "portable-id=${PORTABLE_ID}" >> "${GITHUB_OUTPUT}"
echo "portable-extension=${PORTABLE_EXTENSION}" >> "${GITHUB_OUTPUT}"
echo "portable-php=${PORTABLE_PHP}" >> "${GITHUB_OUTPUT}"
echo "portable-node=${PORTABLE_NODE}" >> "${GITHUB_OUTPUT}"
echo "component-dir=${CONFIG_DIR}" >> "${GITHUB_OUTPUT}"

echo "Config resolved from ${CONFIG_FILE}:"
echo "  id:        ${PORTABLE_ID}"
echo "  extension: ${PORTABLE_EXTENSION:-none}"
echo "  php:       ${PORTABLE_PHP:-skip}"
echo "  node:      ${PORTABLE_NODE:-skip}"
echo "  dir:       ${CONFIG_DIR}"
