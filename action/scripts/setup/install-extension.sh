#!/usr/bin/env bash

set -euo pipefail

EXTENSION_INPUT="${EXTENSION_INPUT:-}"
COMPONENT_DIR="${COMPONENT_DIR:-.}"

if [[ "${EXTENSION_INPUT}" == *,* ]]; then
  echo "::error::Comma-separated extension input is not supported. Declare multiple extensions in homeboy.json instead."
  exit 1
fi

if [ -n "${EXTENSION_INPUT}" ]; then
  echo "Installing extension override: ${EXTENSION_INPUT} from ${EXTENSION_SOURCE}..."
  homeboy extension install "${EXTENSION_SOURCE}" --id "${EXTENSION_INPUT}"
  echo "Extension '${EXTENSION_INPUT}' installed successfully"
else
  if homeboy extension install-for-component --help >/dev/null 2>&1; then
    echo "Installing extensions configured by ${COMPONENT_DIR}/homeboy.json from ${EXTENSION_SOURCE}..."
    homeboy extension install-for-component --path "${COMPONENT_DIR}" --source "${EXTENSION_SOURCE}"
    echo "Configured extensions installed successfully"
  else
    echo "::warning::Installed Homeboy does not support 'extension install-for-component'; falling back to '${EXTENSION_ID}' only"
    homeboy extension install "${EXTENSION_SOURCE}" --id "${EXTENSION_ID}"
    echo "Extension '${EXTENSION_ID}' installed successfully"
  fi
fi
