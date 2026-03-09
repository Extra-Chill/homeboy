#!/usr/bin/env bash

set -euo pipefail

echo "Installing extension: ${EXTENSION_ID} from ${EXTENSION_SOURCE}..."

# Capture the source revision BEFORE install. homeboy extension install from a
# monorepo extracts only the subdirectory and discards .git, so we need to grab
# the HEAD commit hash from the remote now while we can.
EXTENSION_SOURCE_REVISION="unknown"
if [[ "${EXTENSION_SOURCE}" =~ ^https?:// ]] || [[ "${EXTENSION_SOURCE}" =~ ^git@ ]]; then
  EXTENSION_SOURCE_REVISION="$(git ls-remote "${EXTENSION_SOURCE}" HEAD 2>/dev/null | cut -f1 | head -c 7 || echo 'unknown')"
  if [ -z "${EXTENSION_SOURCE_REVISION}" ]; then
    EXTENSION_SOURCE_REVISION="unknown"
  fi
fi
echo "HOMEBOY_EXTENSION_SOURCE_REVISION=${EXTENSION_SOURCE_REVISION}" >> "${GITHUB_ENV}"

homeboy extension install "${EXTENSION_SOURCE}" --id "${EXTENSION_ID}"
echo "Extension '${EXTENSION_ID}' installed successfully (source revision: ${EXTENSION_SOURCE_REVISION})"
