#!/usr/bin/env bash
# Anti-pattern definitions for Homeboy-managed repositories
# Sourced by hook scripts to check for patterns that should use Homeboy commands
#
# SYNC NOTE: TypeScript equivalent at ../opencode/homeboy-plugin.ts
# When modifying patterns, update both files.

# Get homeboy context for navigation guidance
# Returns JSON data or empty string for non-homeboy directories
get_homeboy_context() {
    homeboy init --json 2>/dev/null || echo ""
}

# Format component paths for display
# Arguments: $1 = homeboy init JSON output
format_component_paths() {
    local json="$1"
    echo "$json" | jq -r '
        .data.components // [] |
        map("  \(.id) → \(.path // "unknown")") |
        join("\n")
    ' 2>/dev/null
}

# Check if we're in monorepo root (contains components but no git_root)
# Arguments: $1 = homeboy init JSON output
is_monorepo_root() {
    local json="$1"
    local git_root contained
    git_root=$(echo "$json" | jq -r '.data.context.git_root // empty' 2>/dev/null)
    contained=$(echo "$json" | jq -r '.data.context.contained_components | length' 2>/dev/null)

    [[ -z "$git_root" && "$contained" -gt 0 ]]
}

# Check git commands and provide navigation help
# Arguments: $1 = command string
# Returns: 0 if anti-pattern detected, 1 otherwise
# Outputs: suggestion message if detected
check_git_antipattern() {
    local cmd="$1"

    # Only check commands starting with "git "
    [[ ! "$cmd" =~ ^git[[:space:]] ]] && return 1

    local json paths
    json=$(get_homeboy_context)

    # Non-homeboy directory: pass through
    [[ -z "$json" ]] && return 1

    # Check if in monorepo root (no git_root but has contained components)
    if is_monorepo_root "$json"; then
        paths=$(format_component_paths "$json")
        cat <<EOF
Git Repository Not Found

You're in a project directory, not a git repository.
Component repositories available:
$paths

Options:
  cd <component-path> && git <command>
  homeboy git <command> <component>
  homeboy changes <component>
EOF
        return 0
    fi

    # In a git repo: check for anti-patterns (git status only)
    if [[ "$cmd" =~ ^git[[:space:]]+status ]]; then
        paths=$(format_component_paths "$json")
        cat <<EOF
Use Homeboy for change detection:
  homeboy changes

${paths:+Components:
$paths

}Benefits: Version context, changelog status, component-aware diffs
EOF
        return 0
    fi

    return 1
}

# Check if a bash command matches a build anti-pattern
# Arguments: $1 = command string
# Returns: 0 if anti-pattern detected, 1 otherwise
# Outputs: suggestion message if detected
check_build_antipattern() {
    local cmd="$1"

    # Direct build.sh execution
    if [[ "$cmd" =~ (\./build\.sh|bash[[:space:]]+build\.sh|sh[[:space:]]+build\.sh) ]]; then
        cat <<'EOF'
Build Script Anti-Pattern

Use Homeboy for builds:
  homeboy build <component>

Benefits: Consistent build process, artifact management, validation
EOF
        return 0
    fi

    return 1
}

# Check if a bash command matches a deploy anti-pattern
# Arguments: $1 = command string
# Returns: 0 if anti-pattern detected, 1 otherwise
# Outputs: suggestion message if detected
check_deploy_antipattern() {
    local cmd="$1"

    # rsync to remote servers (basic pattern - excludes local rsync)
    if [[ "$cmd" =~ rsync.*@ ]]; then
        cat <<'EOF'
Deploy Anti-Pattern (rsync)

Use Homeboy for deployments:
  homeboy deploy

Benefits: Server configuration, artifact handling, post-deploy verification
EOF
        return 0
    fi

    # scp to remote servers
    if [[ "$cmd" =~ scp.*@ ]]; then
        cat <<'EOF'
Deploy Anti-Pattern (scp)

Use Homeboy for deployments:
  homeboy deploy

Benefits: Server configuration, artifact handling, post-deploy verification
EOF
        return 0
    fi

    return 1
}

# Check if a bash command matches a version anti-pattern
# Arguments: $1 = command string
# Returns: 0 if anti-pattern detected, 1 otherwise
# Outputs: suggestion message if detected
check_version_antipattern() {
    local cmd="$1"

    # npm version commands
    if [[ "$cmd" =~ npm[[:space:]]+version ]]; then
        cat <<'EOF'
Version Anti-Pattern (npm)

Use Homeboy for version changes:
  homeboy version bump <component> patch|minor|major
  homeboy version set <component> X.Y.Z

Benefits: Automatic changelog, consistent targets, git commit
EOF
        return 0
    fi

    # cargo set-version commands
    if [[ "$cmd" =~ cargo[[:space:]]+set-version ]]; then
        cat <<'EOF'
Version Anti-Pattern (cargo)

Use Homeboy for version changes:
  homeboy version bump <component> patch|minor|major
  homeboy version set <component> X.Y.Z

Benefits: Automatic changelog, consistent targets, git commit
EOF
        return 0
    fi

    return 1
}

# Check if a bash command matches a release anti-pattern (manual git operations)
# Arguments: $1 = command string
# Returns: 0 if anti-pattern detected, 1 otherwise
# Outputs: suggestion message if detected
check_release_antipattern() {
    local cmd="$1"

    # Manual git commit with release message pattern
    if [[ "$cmd" =~ git[[:space:]]+commit.*(-m|--message)[[:space:]]*[\"\']*release: ]]; then
        cat <<'EOF'
Release Anti-Pattern (manual git commit)

Use the unified release pipeline:
  homeboy release <component>

Benefits: Pipeline validation, changelog checks, module-backed publishing
EOF
        return 0
    fi

    # Manual git tag with version pattern (v followed by digit)
    if [[ "$cmd" =~ git[[:space:]]+tag[[:space:]]+v[0-9] ]]; then
        cat <<'EOF'
Release Anti-Pattern (manual git tag)

Use the unified release pipeline:
  homeboy release <component>

Benefits: Tag created after validation, proper commit reference
EOF
        return 0
    fi

    return 1
}

# Check all bash anti-patterns
# Arguments: $1 = command string
# Returns: 0 if any anti-pattern detected, 1 otherwise
# Outputs: suggestion message if detected
check_bash_antipatterns() {
    local cmd="$1"

    check_git_antipattern "$cmd" && return 0
    check_build_antipattern "$cmd" && return 0
    check_deploy_antipattern "$cmd" && return 0
    check_version_antipattern "$cmd" && return 0
    check_release_antipattern "$cmd" && return 0

    return 1
}
