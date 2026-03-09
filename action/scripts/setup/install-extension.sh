#!/usr/bin/env bash

set -euo pipefail

echo "Installing extension: ${EXTENSION_ID} from ${EXTENSION_SOURCE}..."
homeboy extension install "${EXTENSION_SOURCE}" --id "${EXTENSION_ID}"
echo "Extension '${EXTENSION_ID}' installed successfully"
