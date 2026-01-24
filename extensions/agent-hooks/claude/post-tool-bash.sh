#!/usr/bin/env bash
# Claude Code PostToolUse (Bash) hook - Error-based suggestions
# Detects error patterns and provides contextual help
# Exit 0: Informational only (always passes)
# Output: additionalContext with suggestions

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(dirname "$SCRIPT_DIR")/core"

# Source error pattern definitions
source "$CORE_DIR/error-patterns.sh"

# Read hook input from stdin (JSON)
hook_input=$(cat)

# Extract tool response fields
# PostToolUse receives: { "tool_name": "Bash", "tool_input": {...}, "tool_response": {...} }
stderr=$(echo "$hook_input" | jq -r '.tool_response.stderr // empty' 2>/dev/null || true)
exit_code=$(echo "$hook_input" | jq -r '.tool_response.exitCode // "0"' 2>/dev/null || true)

# Analyze the error and get suggestions
suggestion=$(analyze_bash_error "$stderr" "$exit_code")

if [[ -n "$suggestion" ]]; then
    # Output suggestion as additionalContext (shown to Claude)
    echo "$suggestion"
fi

exit 0
