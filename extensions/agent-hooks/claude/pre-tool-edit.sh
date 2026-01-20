#!/usr/bin/env bash
# Claude Code PreToolUse (Edit) hook - Dynamic file protection
# Uses homeboy init --json to detect protected files
# Exit 0: Allow edit
# Exit 2: Block edit with reason

set -euo pipefail

# Read tool input from stdin (JSON)
tool_input=$(cat)

# Extract file path from tool input
file_path=$(echo "$tool_input" | jq -r '.tool_input.file_path // empty' 2>/dev/null || true)

[[ -z "$file_path" ]] && exit 0

# Get protected files from Homeboy (suppress errors for non-Homeboy repos)
init_data=$(homeboy init --json 2>/dev/null | jq '.data' 2>/dev/null || echo "{}")

# Check changelog protection
changelog_path=$(echo "$init_data" | jq -r '.changelog.path // empty' 2>/dev/null || true)
if [[ -n "$changelog_path" && "$file_path" == "$changelog_path" ]]; then
    cat <<'EOF'
Changelog Protection

Use Homeboy for changelog entries:
  homeboy changelog add

This ensures proper formatting and version association.
EOF
    exit 2
fi

# Check version targets protection
version_files=$(echo "$init_data" | jq -r '.version.targets[].full_path // empty' 2>/dev/null || true)
for version_file in $version_files; do
    if [[ "$file_path" == "$version_file" ]]; then
        cat <<'EOF'
Version File Protection

Use Homeboy for version changes:
  homeboy version bump <component> patch|minor|major
  homeboy version set <component> X.Y.Z

Benefits: Automatic changelog, consistent targets, git commit
EOF
        exit 2
    fi
done

# No protected files matched, allow edit
exit 0
