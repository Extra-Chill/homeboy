#!/usr/bin/env bash
# Agent Hooks uninstall script
# Removes Claude Code hooks and cleans settings.json

set -euo pipefail

CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks/agent-hooks"
SETTINGS_FILE="$CLAUDE_DIR/settings.json"

echo "Uninstalling Agent Hooks..."

# Remove hooks directory
if [[ -d "$HOOKS_DIR" ]]; then
    rm -rf "$HOOKS_DIR"
    echo "Removed hooks directory: $HOOKS_DIR"
else
    echo "Hooks directory not found (already removed?)"
fi

# Clean up settings.json
if [[ -f "$SETTINGS_FILE" ]]; then
    echo "Cleaning up Claude Code settings..."

    # Remove agent-hooks entries from settings
    cleaned_settings=$(cat "$SETTINGS_FILE" | jq '
        # Remove SessionStart hooks that reference agent-hooks
        .hooks.SessionStart = (
            .hooks.SessionStart // [] |
            map(select(
                .hooks == null or
                (.hooks | map(select(.command | contains("agent-hooks"))) | length == 0)
            ))
        ) |

        # Remove PreToolUse hooks that reference agent-hooks
        .hooks.PreToolUse = (
            .hooks.PreToolUse // [] |
            map(select(
                .hooks == null or
                (.hooks | map(select(.command | contains("agent-hooks"))) | length == 0)
            ))
        ) |

        # Clean up empty arrays
        if .hooks.SessionStart == [] then del(.hooks.SessionStart) else . end |
        if .hooks.PreToolUse == [] then del(.hooks.PreToolUse) else . end |
        if .hooks == {} then del(.hooks) else . end
    ')

    echo "$cleaned_settings" > "$SETTINGS_FILE"
    echo "Settings cleaned."
else
    echo "Settings file not found (nothing to clean)"
fi

# Remove empty hooks directory if parent exists
if [[ -d "$CLAUDE_DIR/hooks" ]]; then
    rmdir "$CLAUDE_DIR/hooks" 2>/dev/null || true
fi

echo ""
echo "Agent Hooks uninstalled successfully."
