#!/usr/bin/env bash
# Claude Code SessionStart hook - Homeboy init reminder
# Exit 0: Informational only (always passes)

set -euo pipefail

# Read session data from stdin (JSON)
cat >/dev/null

# Output reminder message
cat <<'EOF'
Homeboy Active

Start with: homeboy init
This gathers context (components, servers, versions) before operations.

Use Homeboy for: builds, deploys, changelogs, version management
Docs: homeboy docs commands/commands-index
EOF

exit 0
