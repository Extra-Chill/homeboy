#!/usr/bin/env bash

set -euo pipefail

echo "Fetching base commit (${BASE_SHA:0:8})..."
git fetch origin "${BASE_SHA}" --depth=1 2>/dev/null || true

echo "base-ref=${BASE_SHA}" >> "${GITHUB_OUTPUT}"
echo "HOMEBOY_CHANGED_SINCE=${BASE_SHA}" >> "${GITHUB_ENV}"
