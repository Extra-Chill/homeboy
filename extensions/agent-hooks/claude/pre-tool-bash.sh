#!/usr/bin/env bash
# Claude Code PreToolUse (Bash) hook - Anti-pattern detector
# Exit 0: Allow command
# Exit 2: Block command with reason

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(dirname "$SCRIPT_DIR")/core"

# Source pattern definitions
source "$CORE_DIR/patterns.sh"

# Read tool input from stdin (JSON)
tool_input=$(cat)

# Extract command from tool input
command=$(echo "$tool_input" | jq -r '.tool_input.command // empty' 2>/dev/null || true)

if [[ -z "$command" ]]; then
    # No command found, allow
    exit 0
fi

# Check for anti-patterns
if message=$(check_bash_antipatterns "$command"); then
    # Anti-pattern detected, block with message
    echo "$message"
    exit 2
fi

# No anti-patterns detected, allow
exit 0
