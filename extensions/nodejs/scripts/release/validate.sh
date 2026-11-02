#!/usr/bin/env bash
set -euo pipefail

# Validate the npm package before publishing.
# Runs `npm run validate` if a validate script is defined in package.json.
# Falls back to `npm pack --dry-run` to verify the package is publishable.

if [[ ! -f "package.json" ]]; then
  echo "No package.json found in $(pwd)" >&2
  exit 1
fi

# Private workspaces do not need publish validation.
if [[ "$(node -p "require('./package.json').private===true")" == "true" ]]; then
  PACKAGE_NAME=$(node -p "require('./package.json').name")
  echo "Skipping npm publish validation: $PACKAGE_NAME is private" >&2
  exit 0
fi

source "$(dirname "${BASH_SOURCE[0]}")/../lib/project-scripts.sh"
homeboy_project_ensure_workspace_dependencies

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
