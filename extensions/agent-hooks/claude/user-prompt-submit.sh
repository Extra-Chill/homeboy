#!/usr/bin/env bash
# Claude Code UserPromptSubmit hook - Topic-based workflow reminders
# Detects keywords in user messages and provides contextual guidance
# Exit 0: Informational only (always passes)
# Output: additionalContext with workflow reminders

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(dirname "$SCRIPT_DIR")/core"

# Source topic pattern definitions
source "$CORE_DIR/topic-patterns.sh"

# Read hook input from stdin (JSON)
hook_input=$(cat)

# Extract user prompt from hook input
# UserPromptSubmit receives: { "prompt": "user message text" }
user_prompt=$(echo "$hook_input" | jq -r '.prompt // empty' 2>/dev/null || true)

if [[ -z "$user_prompt" ]]; then
    # No prompt found, pass through
    exit 0
fi

# Check for topic keywords and get reminders
reminders=$(check_all_topics "$user_prompt")

if [[ -n "$reminders" ]]; then
    # Output reminders as additionalContext (shown to Claude)
    echo "$reminders"
fi

exit 0
