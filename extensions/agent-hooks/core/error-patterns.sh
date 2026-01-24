#!/usr/bin/env bash
# Error detection patterns for tool response analysis
# Sourced by post-tool-bash.sh to provide contextual suggestions
#
# SYNC NOTE: TypeScript equivalent at ../opencode/homeboy-plugin.ts
# When modifying patterns, update both files.

# Source patterns.sh for homeboy context helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/patterns.sh"

# Check if stderr contains git repository not found error
# Arguments: $1 = stderr content
# Returns: 0 if detected, 1 otherwise
check_git_not_found_error() {
    local stderr="$1"

    # Match: "not a git repository", "fatal: not a git repository"
    if [[ "$stderr" =~ not[[:space:]]a[[:space:]]git[[:space:]]repository ]]; then
        return 0
    fi
    return 1
}

# Check if we're in a monorepo context where git errors make sense
# Returns: 0 if in monorepo root, 1 otherwise
is_in_monorepo_context() {
    local json
    json=$(get_homeboy_context)

    # Non-homeboy directory: not a monorepo context
    [[ -z "$json" ]] && return 1

    is_monorepo_root "$json"
}

# Get suggestion for git not found error in monorepo context
get_git_not_found_suggestion() {
    local json
    json=$(get_homeboy_context)

    if [[ -z "$json" ]]; then
        return
    fi

    local paths
    paths=$(format_component_paths "$json")

    cat <<EOF
Git Repository Context

Run 'homeboy init' to understand your project structure.
This appears to be a monorepo - components have their own git repos.
${paths:+
Component repositories:
$paths}
EOF
}

# Analyze tool response and return contextual suggestions
# Arguments: $1 = stderr, $2 = exit_code
# Returns: suggestion message (may be empty)
analyze_bash_error() {
    local stderr="$1"
    local exit_code="$2"

    # Only analyze failed commands
    [[ "$exit_code" == "0" ]] && return

    # Check for git not found error in monorepo context
    if check_git_not_found_error "$stderr" && is_in_monorepo_context; then
        get_git_not_found_suggestion
        return
    fi

    # No specific suggestion for this error
    echo ""
}
