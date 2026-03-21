#!/usr/bin/env bash
set -euo pipefail

# Build the npm package before publishing.
# Runs `npm run build` if a build script is defined in package.json.
# Skips gracefully if no build script exists.

if [[ ! -f "package.json" ]]; then
  echo "No package.json found in $(pwd)" >&2
  exit 1
fi

# Check if a build script exists
has_build=$(node -e "
  const pkg = require('./package.json');
  console.log(pkg.scripts && pkg.scripts.build ? 'yes' : 'no');
" 2>/dev/null || echo "no")

if [[ "$has_build" == "yes" ]]; then
  echo "Running npm run build..."
  npm run build
else
  echo "No build script defined in package.json, skipping build"
fi
