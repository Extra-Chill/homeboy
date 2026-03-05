#!/usr/bin/env bash

set -euo pipefail

if [ ! -f "homeboy.json" ]; then
  echo "::error::homeboy.json is required at repository root for Homeboy Action"
  echo "::error::Create homeboy.json (portable config) and re-run CI"
  exit 1
fi

PORTABLE_ID="$(jq -r '.id // empty' homeboy.json 2>/dev/null || true)"
if [ -z "${PORTABLE_ID}" ]; then
  echo "::error::homeboy.json must include a top-level \"id\" for CI component identity"
  exit 1
fi

echo "Portable config detected: homeboy.json (id=${PORTABLE_ID})"
