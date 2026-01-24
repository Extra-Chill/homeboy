#!/usr/bin/env bash
# Claude Code SessionStart hook - Auto-init context injection
# Exit 0: Informational only (always passes)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Read session data from stdin (JSON)
cat >/dev/null

# Run homeboy init and capture output
if INIT_OUTPUT=$(homeboy init 2>/dev/null) && [ -n "$INIT_OUTPUT" ]; then
    echo "Homeboy Active (auto-init)"
    echo ""
    echo "$INIT_OUTPUT"
else
    # Fallback to static message if homeboy not available
    cat "$SCRIPT_DIR/../core/session-message.txt"
fi

exit 0
