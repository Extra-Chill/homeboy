#!/usr/bin/env bash

set -euo pipefail

if homeboy --help 2>&1 | grep -E '^[[:space:]]+--placement([[:space:]]|<)' >/dev/null; then
  supports_placement=true
else
  supports_placement=false
fi

echo "HOMEBOY_ACTION_SUPPORTS_PLACEMENT=${supports_placement}" >> "${GITHUB_ENV}"
