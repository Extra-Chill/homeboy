#!/usr/bin/env bash
set -euo pipefail

# Validate the npm package before publishing.
# Runs `npm run validate` if a validate script is defined in package.json.
# Falls back to `npm pack --dry-run` to verify the package is publishable.

if [[ ! -f "package.json" ]]; then
  echo "No package.json found in $(pwd)" >&2
  exit 1
fi

# Check if a validate script exists
has_validate=$(node -e "
  const pkg = require('./package.json');
  console.log(pkg.scripts && pkg.scripts.validate ? 'yes' : 'no');
" 2>/dev/null || echo "no")

if [[ "$has_validate" == "yes" ]]; then
  echo "Running npm run validate..." >&2
  npm run validate >&2
else
  echo "No validate script defined, running npm pack --dry-run..." >&2
  npm pack --dry-run >&2
fi
