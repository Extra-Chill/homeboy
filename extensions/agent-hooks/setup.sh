#!/usr/bin/env bash
# Agent Hooks setup script
# Installs Claude Code hooks and configures settings.json

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks/agent-hooks"
SETTINGS_FILE="$CLAUDE_DIR/settings.json"

echo "Installing Agent Hooks for Claude Code..."

# Create Claude directories if they don't exist
mkdir -p "$HOOKS_DIR"

# Copy hook scripts
echo "Copying hooks to $HOOKS_DIR..."
cp -r "$SCRIPT_DIR/core" "$HOOKS_DIR/"
cp -r "$SCRIPT_DIR/claude" "$HOOKS_DIR/"

# Make scripts executable
chmod +x "$HOOKS_DIR/core/"*.sh
chmod +x "$HOOKS_DIR/claude/"*.sh

echo "Hooks installed successfully."

# Configure settings.json
echo "Configuring Claude Code settings..."

# Hook configuration to merge
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
      }
    ]
  }
}
EOF
)

# Check if settings.json exists
if [[ -f "$SETTINGS_FILE" ]]; then
    # Backup existing settings
    cp "$SETTINGS_FILE" "$SETTINGS_FILE.backup"
    echo "Backed up existing settings to $SETTINGS_FILE.backup"

    # Read existing settings
    existing_settings=$(cat "$SETTINGS_FILE")

    # Merge hooks configuration using jq
    # This adds our hooks to existing hooks arrays, or creates them if they don't exist
    merged_settings=$(echo "$existing_settings" | jq --argjson new_hooks "$HOOKS_CONFIG" '
        # Deep merge function for hooks
        def merge_hook_arrays($existing; $new):
            if $existing == null then $new
            elif $new == null then $existing
            else $existing + $new
            end;

        # Merge SessionStart hooks
        .hooks.SessionStart = merge_hook_arrays(.hooks.SessionStart; $new_hooks.hooks.SessionStart) |

        # Merge PreToolUse hooks
        .hooks.PreToolUse = merge_hook_arrays(.hooks.PreToolUse; $new_hooks.hooks.PreToolUse)
    ')

    echo "$merged_settings" > "$SETTINGS_FILE"
else
    # Create new settings file with just hooks
    mkdir -p "$CLAUDE_DIR"
    echo "$HOOKS_CONFIG" > "$SETTINGS_FILE"
fi

echo "Settings configured successfully."
echo ""
echo "Agent Hooks installation complete!"
echo "Hooks are now active for Claude Code sessions."
echo ""
echo "To verify: cat ~/.claude/settings.json"
echo "To uninstall: homeboy module run agent-hooks uninstall"
