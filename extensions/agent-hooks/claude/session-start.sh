#!/usr/bin/env bash
# Claude Code SessionStart hook - Homeboy init reminder
# Exit 0: Informational only (always passes)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Read session data from stdin (JSON)
cat >/dev/null

# Output reminder message from centralized source
cat "$SCRIPT_DIR/../core/session-message.txt"

exit 0
