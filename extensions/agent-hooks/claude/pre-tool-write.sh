#!/usr/bin/env bash
# Claude Code PreToolUse (Write) hook - Dynamic file protection
# Uses homeboy init --json to detect protected files
# Exit 0: Allow write
# Exit 2: Block write with reason

set -euo pipefail

# Read tool input from stdin (JSON)
tool_input=$(cat)

# Extract file path from tool input
file_path=$(echo "$tool_input" | jq -r '.tool_input.file_path // empty' 2>/dev/null || true)

[[ -z "$file_path" ]] && exit 0

# Find the project root for this file by walking up the directory tree
find_project_root() {
    local path="$1"
    local dir

    # Start from parent directory (handles both existing files and new files)
    dir=$(dirname "$path")

    # Walk up looking for git root
    while [[ "$dir" != "/" && "$dir" != "." ]]; do
        if [[ -d "$dir/.git" ]]; then
            echo "$dir"
            return 0
        fi
        dir=$(dirname "$dir")
    done

    return 1
}

project_root=$(find_project_root "$file_path" 2>/dev/null || true)
[[ -z "$project_root" ]] && exit 0

# Get protected files from Homeboy (run from project root, suppress errors for non-Homeboy repos)
init_data=$(cd "$project_root" && homeboy init --json 2>/dev/null | jq '.data' 2>/dev/null || echo "{}")

# Check changelog protection
changelog_path=$(echo "$init_data" | jq -r '.changelog.path // empty' 2>/dev/null || true)
if [[ -n "$changelog_path" && "$file_path" == "$changelog_path" ]]; then
    cat <<'EOF' >&2
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
        cat <<'EOF' >&2
Version File Protection

Use Homeboy for version changes:
  homeboy version bump <component> patch|minor|major
  homeboy version set <component> X.Y.Z

Benefits: Automatic changelog, consistent targets, git commit
EOF
        exit 2
    fi
done

# No protected files matched, allow write
exit 0
