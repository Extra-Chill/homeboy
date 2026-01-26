#!/usr/bin/env bash
# Agent Hooks setup script
# Installs hooks for Claude Code and OpenCode

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ============================================================================
# Shared Config Installation
# ============================================================================
install_shared() {
    local CONFIG_DIR="$HOME/.config/homeboy"
    local MESSAGE_FILE="$CONFIG_DIR/agent-message.txt"

    echo "Installing shared agent configuration..."

    # Create config directory
    mkdir -p "$CONFIG_DIR"

    # Copy centralized session message
    cp "$SCRIPT_DIR/core/session-message.txt" "$MESSAGE_FILE"

    echo "  Message installed to $MESSAGE_FILE"
}

# ============================================================================
# Claude Code Installation
# ============================================================================
install_claude() {
    local CLAUDE_DIR="$HOME/.claude"
    local HOOKS_DIR="$CLAUDE_DIR/hooks/agent-hooks"
    local SETTINGS_FILE="$CLAUDE_DIR/settings.json"

    echo "Installing Agent Hooks for Claude Code..."

    # Create directories
    mkdir -p "$HOOKS_DIR"

    # Copy hook scripts
    cp -r "$SCRIPT_DIR/core" "$HOOKS_DIR/"
    cp -r "$SCRIPT_DIR/claude" "$HOOKS_DIR/"

    # Make scripts executable
    chmod +x "$HOOKS_DIR/core/"*.sh
    chmod +x "$HOOKS_DIR/claude/"*.sh

    echo "  Hooks copied to $HOOKS_DIR"

    # Hook configuration to add
    local HOOKS_CONFIG
    HOOKS_CONFIG=$(cat <<'EOF'
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/agent-hooks/claude/session-start.sh"
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/agent-hooks/claude/user-prompt-submit.sh"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/agent-hooks/claude/pre-tool-bash.sh"
          }
        ]
      },
      {
        "matcher": "Edit",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/agent-hooks/claude/pre-tool-edit.sh"
          }
        ]
      },
      {
        "matcher": "Write",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/agent-hooks/claude/pre-tool-write.sh"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/agent-hooks/claude/post-tool-bash.sh"
          }
        ]
      }
    ]
  }
}
EOF
)

    # Merge or create settings.json using replace strategy
    # First remove any existing agent-hooks entries, then add fresh config
    if [[ -f "$SETTINGS_FILE" ]]; then
        cp "$SETTINGS_FILE" "$SETTINGS_FILE.backup"
        echo "  Backed up existing settings to $SETTINGS_FILE.backup"

        local merged
        merged=$(cat "$SETTINGS_FILE" | jq --argjson new_hooks "$HOOKS_CONFIG" '
            # Helper to filter out agent-hooks entries from an array
            def remove_agent_hooks:
                if . == null then null
                else map(select(
                    (.hooks // []) | all(.command | (. == null) or (contains("agent-hooks") | not))
                ))
                end;

            # Helper to merge arrays (existing cleaned + new)
            def merge_arrays($existing; $new):
                if $existing == null then $new
                elif $new == null then $existing
                else ($existing | remove_agent_hooks) + $new
                end;

            # Apply to all hook types
            .hooks.SessionStart = merge_arrays(.hooks.SessionStart; $new_hooks.hooks.SessionStart) |
            .hooks.UserPromptSubmit = merge_arrays(.hooks.UserPromptSubmit; $new_hooks.hooks.UserPromptSubmit) |
            .hooks.PreToolUse = merge_arrays(.hooks.PreToolUse; $new_hooks.hooks.PreToolUse) |
            .hooks.PostToolUse = merge_arrays(.hooks.PostToolUse; $new_hooks.hooks.PostToolUse)
        ')
        echo "$merged" > "$SETTINGS_FILE"
    else
        mkdir -p "$CLAUDE_DIR"
        echo "$HOOKS_CONFIG" > "$SETTINGS_FILE"
    fi

    echo "  Settings configured"
    echo "Claude Code hooks installed."
}

# ============================================================================
# OpenCode Installation
# ============================================================================
install_opencode() {
    local OPENCODE_PLUGINS_DIR="$HOME/.config/opencode/plugins"

    echo "Installing Agent Hooks for OpenCode..."

    # Create plugins directory
    mkdir -p "$OPENCODE_PLUGINS_DIR"

    # Copy plugin file
    cp "$SCRIPT_DIR/opencode/homeboy-plugin.ts" "$OPENCODE_PLUGINS_DIR/"

    echo "  Plugin copied to $OPENCODE_PLUGINS_DIR/homeboy-plugin.ts"
    echo "OpenCode plugin installed."
}

# ============================================================================
# Main
# ============================================================================
install_shared
echo ""
install_claude
echo ""
install_opencode

echo ""
echo "Agent Hooks installation complete!"
echo ""
echo "Installed for:"
echo "  - Shared config: ~/.config/homeboy/agent-message.txt"
echo "  - Claude Code: ~/.claude/hooks/agent-hooks/"
echo "  - OpenCode: ~/.config/opencode/plugins/homeboy-plugin.ts"
echo ""
echo "To verify Claude: cat ~/.claude/settings.json"
echo "To uninstall: homeboy module run agent-hooks uninstall"
