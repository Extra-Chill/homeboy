#!/usr/bin/env bash
# Agent Hooks uninstall script
# Removes hooks for Claude Code and OpenCode

set -euo pipefail

# ============================================================================
# Claude Code Uninstallation
# ============================================================================
uninstall_claude() {
    local CLAUDE_DIR="$HOME/.claude"
    local HOOKS_DIR="$CLAUDE_DIR/hooks/agent-hooks"
    local SETTINGS_FILE="$CLAUDE_DIR/settings.json"

    echo "Uninstalling Claude Code hooks..."

    # Remove hooks directory
    if [[ -d "$HOOKS_DIR" ]]; then
        rm -rf "$HOOKS_DIR"
        echo "  Removed: $HOOKS_DIR"
    else
        echo "  Hooks directory not found (already removed?)"
    fi

    # Clean up settings.json
    if [[ -f "$SETTINGS_FILE" ]]; then
        local cleaned
        cleaned=$(cat "$SETTINGS_FILE" | jq '
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
        echo "$cleaned" > "$SETTINGS_FILE"
        echo "  Cleaned: settings.json"
    else
        echo "  Settings file not found (nothing to clean)"
    fi

    # Remove empty hooks directory
    [[ -d "$CLAUDE_DIR/hooks" ]] && rmdir "$CLAUDE_DIR/hooks" 2>/dev/null || true

    echo "Claude Code hooks uninstalled."
}

# ============================================================================
# OpenCode Uninstallation
# ============================================================================
uninstall_opencode() {
    local PLUGIN_FILE="$HOME/.config/opencode/plugins/homeboy-plugin.ts"

    echo "Uninstalling OpenCode plugin..."

    if [[ -f "$PLUGIN_FILE" ]]; then
        rm "$PLUGIN_FILE"
        echo "  Removed: $PLUGIN_FILE"
    else
        echo "  Plugin file not found (already removed?)"
    fi

    echo "OpenCode plugin uninstalled."
}

# ============================================================================
# Shared Config Uninstallation
# ============================================================================
uninstall_shared() {
    local MESSAGE_FILE="$HOME/.config/homeboy/agent-message.txt"

    echo "Removing shared agent configuration..."

    if [[ -f "$MESSAGE_FILE" ]]; then
        rm "$MESSAGE_FILE"
        echo "  Removed: $MESSAGE_FILE"
    else
        echo "  Message file not found (already removed?)"
    fi
}

# ============================================================================
# Main
# ============================================================================
uninstall_claude
echo ""
uninstall_opencode
echo ""
uninstall_shared

echo ""
echo "Agent Hooks uninstalled successfully."
