#!/usr/bin/env bash

# Detect runtime environment requirements via homeboy component env.
#
# Runs after homeboy is installed. Uses the extension's native detection
# logic — for WordPress, this reads "Requires PHP" from the plugin/theme
# file header instead of requiring manual configuration.
#
# Action input overrides (PHP_INPUT, NODE_INPUT) take priority over
# detected values.
#
# Outputs (GITHUB_ENV + GITHUB_OUTPUT):
#   PORTABLE_PHP  — PHP version to install
#   PORTABLE_NODE — Node version to install

set -euo pipefail

PHP_INPUT="${PHP_INPUT:-}"
NODE_INPUT="${NODE_INPUT:-}"
COMPONENT_DIR="${COMPONENT_DIR:-.}"

PORTABLE_PHP=""
PORTABLE_NODE=""

# Run homeboy component env to detect versions from source files
ENV_JSON=""
if command -v homeboy &>/dev/null; then
  ENV_JSON="$(homeboy component env --path "${COMPONENT_DIR}" --output /dev/null 2>/dev/null || true)"
fi

if [ -n "${ENV_JSON}" ]; then
  DETECTED_PHP="$(echo "${ENV_JSON}" | jq -r '.data.entity.php // empty' 2>/dev/null || true)"
  DETECTED_NODE="$(echo "${ENV_JSON}" | jq -r '.data.entity.node // empty' 2>/dev/null || true)"
else
  DETECTED_PHP=""
  DETECTED_NODE=""
fi

# Action input overrides take priority
if [ -n "${PHP_INPUT}" ]; then
  PORTABLE_PHP="${PHP_INPUT}"
elif [ -n "${DETECTED_PHP}" ]; then
  PORTABLE_PHP="${DETECTED_PHP}"
fi

if [ -n "${NODE_INPUT}" ]; then
  PORTABLE_NODE="${NODE_INPUT}"
elif [ -n "${DETECTED_NODE}" ]; then
  PORTABLE_NODE="${DETECTED_NODE}"
fi

echo "PORTABLE_PHP=${PORTABLE_PHP}" >> "${GITHUB_ENV}"
echo "PORTABLE_NODE=${PORTABLE_NODE}" >> "${GITHUB_ENV}"

echo "portable-php=${PORTABLE_PHP}" >> "${GITHUB_OUTPUT}"
echo "portable-node=${PORTABLE_NODE}" >> "${GITHUB_OUTPUT}"

echo "Runtime env detected:"
echo "  php:  ${PORTABLE_PHP:-skip}${PHP_INPUT:+ (overridden by input)}"
echo "  node: ${PORTABLE_NODE:-skip}${NODE_INPUT:+ (overridden by input)}"
