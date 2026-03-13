#!/usr/bin/env bash
set -euo pipefail

# Dependency resolution for WordPress extension validation.
#
# Resolves dependencies from two sources (merged, deduplicated):
#   1. Plugin header "Requires Plugins:" (auto-discovered, zero-config)
#   2. Settings JSON "validation_dependencies" / "depends_on" (manual overrides)
#
# Resolution chain for each dependency slug:
#   1. Direct path (if the value is an existing directory)
#   2. homeboy component show → local_path (if homeboy is available)
#   3. Git clone from GitHub org (shallow, cached across steps)
#      Org inferred from: HOMEBOY_DEPENDENCY_GITHUB_ORG → git remote origin
#   4. Warn and skip
#
# Settings deps take priority — they can be absolute paths or slugs.
# Header deps are resolved through the same chain by slug.

homeboy_get_validation_dependencies_raw() {
    local settings_json="${HOMEBOY_SETTINGS_JSON:-}"

    if [ -z "$settings_json" ] || [ "$settings_json" = "{}" ]; then
        return 0
    fi

    printf '%s' "$settings_json" | jq -r '.validation_dependencies // .depends_on // empty' 2>/dev/null || true
}

homeboy_normalize_validation_dependencies() {
    local raw="${1:-}"

    if [ -z "$raw" ] || [ "$raw" = "null" ]; then
        return 0
    fi

    if printf '%s' "$raw" | jq -e 'type == "array"' >/dev/null 2>&1; then
        printf '%s' "$raw" | jq -r '.[]'
        return 0
    fi

    if printf '%s' "$raw" | jq -e 'type == "string"' >/dev/null 2>&1; then
        raw=$(printf '%s' "$raw" | jq -r '.')
    fi

    raw=${raw//,/\n}

    while IFS= read -r entry; do
        entry="${entry#${entry%%[![:space:]]*}}"
        entry="${entry%${entry##*[![:space:]]}}"
        [ -n "$entry" ] && printf '%s\n' "$entry"
    done <<< "$raw"
}

# Parse "Requires Plugins:" header from a plugin's main PHP file.
# WordPress format: comma-separated slugs, e.g. "plugin-a, plugin-b"
# Returns one slug per line (trimmed, lowercased).
homeboy_get_requires_plugins_from_header() {
    local plugin_path="${1:-}"

    [ -z "$plugin_path" ] || [ ! -d "$plugin_path" ] && return 0

    # Find the main plugin file (*.php with "Plugin Name:" in root)
    local main_file
    main_file=$(find "$plugin_path" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)

    [ -z "$main_file" ] && return 0

    local requires_line
    requires_line=$(grep -m1 "Requires Plugins:" "$main_file" 2>/dev/null | sed 's/.*Requires Plugins:[[:space:]]*//' | sed 's/[[:space:]]*$//' | tr -d '\r')

    [ -z "$requires_line" ] && return 0

    # Split comma-separated slugs, trim whitespace, output one per line
    local IFS=','
    local slug
    for slug in $requires_line; do
        slug="${slug#${slug%%[![:space:]]*}}"
        slug="${slug%${slug##*[![:space:]]}}"
        [ -n "$slug" ] && printf '%s\n' "$slug"
    done
}

# Infer GitHub org from a git repo's remote.
# Parses "origin" remote URL to extract the org/owner.
# Arg: optional directory to read from (defaults to cwd).
# Returns empty string if not a git repo or can't parse.
_homeboy_infer_github_org() {
    local repo_dir="${1:-.}"
    local remote_url
    remote_url=$(git -C "$repo_dir" remote get-url origin 2>/dev/null || true)

    [ -z "$remote_url" ] && return 0

    # Handle HTTPS and SSH: github.com/ORG/repo or github.com:ORG/repo
    if [[ "$remote_url" =~ github\.com[/:]([^/]+)/ ]]; then
        printf '%s\n' "${BASH_REMATCH[1]}"
        return 0
    fi

    return 0
}

# Get the dependency cache directory for git-cloned deps.
# Uses HOMEBOY_CACHE_DIR/deps/ if set, otherwise a temp directory.
# The directory is created on first use and reused across calls.
_homeboy_get_dep_cache_dir() {
    if [ -n "${_HOMEBOY_DEP_CACHE_DIR:-}" ] && [ -d "$_HOMEBOY_DEP_CACHE_DIR" ]; then
        printf '%s\n' "$_HOMEBOY_DEP_CACHE_DIR"
        return 0
    fi

    local base_dir="${HOMEBOY_CACHE_DIR:-${TMPDIR:-/tmp}}"
    _HOMEBOY_DEP_CACHE_DIR="${base_dir}/homeboy-deps"
    mkdir -p "$_HOMEBOY_DEP_CACHE_DIR" 2>/dev/null || true

    if [ -d "$_HOMEBOY_DEP_CACHE_DIR" ]; then
        printf '%s\n' "$_HOMEBOY_DEP_CACHE_DIR"
        return 0
    fi

    return 1
}

# Clone a dependency from GitHub by slug.
# Uses shallow clone (--depth 1) for speed.
# Returns the clone path if successful, empty otherwise.
_homeboy_clone_dependency() {
    local slug="${1:-}"
    local github_org="${2:-}"

    [ -z "$slug" ] || [ -z "$github_org" ] && return 1

    local cache_dir
    cache_dir=$(_homeboy_get_dep_cache_dir || true)
    [ -z "$cache_dir" ] && return 1

    local clone_path="${cache_dir}/${slug}"

    # Already cloned in this session — reuse
    if [ -d "$clone_path" ]; then
        printf '%s\n' "$clone_path"
        return 0
    fi

    # Build repo URL — use token auth if GITHUB_TOKEN is available (CI environments)
    local repo_url
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        repo_url="https://x-access-token:${GITHUB_TOKEN}@github.com/${github_org}/${slug}.git"
    else
        repo_url="https://github.com/${github_org}/${slug}.git"
    fi

    if git clone --depth 1 --quiet "$repo_url" "$clone_path" 2>/dev/null; then
        printf '%s\n' "$clone_path"
        return 0
    fi

    # Clone failed — clean up partial clone
    rm -rf "$clone_path" 2>/dev/null || true
    return 1
}

homeboy_resolve_validation_dependency_path() {
    local dependency="${1:-}"

    [ -z "$dependency" ] && return 1

    # 1. Direct path (absolute or relative directory)
    if [ -d "$dependency" ]; then
        printf '%s\n' "$dependency"
        return 0
    fi

    # 2. Homeboy component registry lookup
    if command -v homeboy >/dev/null 2>&1; then
        local resolved
        resolved=$(homeboy component show "$dependency" 2>/dev/null | jq -r '.data.entity.local_path // empty' 2>/dev/null || true)
        if [ -n "$resolved" ] && [ -d "$resolved" ]; then
            printf '%s\n' "$resolved"
            return 0
        fi
    fi

    # 3. Git clone from GitHub org (for CI environments)
    #    Only attempt for slug-like values (no slashes, no absolute paths)
    if [[ "$dependency" != */* ]] && command -v git >/dev/null 2>&1; then
        local github_org="${HOMEBOY_DEPENDENCY_GITHUB_ORG:-}"

        if [ -z "$github_org" ]; then
            # Infer from the component being validated (passed via _HOMEBOY_DEP_PLUGIN_PATH)
            github_org=$(_homeboy_infer_github_org "${_HOMEBOY_DEP_PLUGIN_PATH:-.}" || true)
        fi

        if [ -n "$github_org" ]; then
            local cloned_path
            cloned_path=$(_homeboy_clone_dependency "$dependency" "$github_org" || true)
            if [ -n "$cloned_path" ] && [ -d "$cloned_path" ]; then
                echo "Resolved dependency '$dependency' via git clone from ${github_org}/${dependency}" >&2
                printf '%s\n' "$cloned_path"
                return 0
            fi
        fi
    fi

    return 1
}

homeboy_resolve_validation_dependency_paths() {
    local plugin_path="${1:-}"

    # Make plugin path available to the resolver for org inference
    _HOMEBOY_DEP_PLUGIN_PATH="$plugin_path"

    # Collect all dependency identifiers from both sources
    local all_deps=""

    # Source 1: Requires Plugins header (auto-discovered)
    local header_deps
    header_deps=$(homeboy_get_requires_plugins_from_header "$plugin_path" || true)
    if [ -n "$header_deps" ]; then
        all_deps="$header_deps"
    fi

    # Source 2: Settings JSON (manual overrides)
    local settings_raw
    settings_raw=$(homeboy_get_validation_dependencies_raw)
    if [ -n "$settings_raw" ]; then
        local settings_deps
        settings_deps=$(homeboy_normalize_validation_dependencies "$settings_raw")
        if [ -n "$settings_deps" ]; then
            if [ -n "$all_deps" ]; then
                all_deps="${all_deps}"$'\n'"${settings_deps}"
            else
                all_deps="$settings_deps"
            fi
        fi
    fi

    [ -z "$all_deps" ] && return 0

    # Deduplicate (settings deps come after header deps, so they win on path resolution)
    # Track resolved paths to avoid duplicates
    local -A seen_paths=()

    while IFS= read -r dependency; do
        [ -z "$dependency" ] && continue

        local resolved
        resolved=$(homeboy_resolve_validation_dependency_path "$dependency" || true)

        if [ -z "$resolved" ]; then
            echo "Warning: Could not resolve WordPress validation dependency '$dependency'" >&2
            continue
        fi

        if [ -n "$plugin_path" ] && [ "$resolved" = "$plugin_path" ]; then
            continue
        fi

        # Deduplicate by resolved path
        if [ -n "${seen_paths[$resolved]+x}" ]; then
            continue
        fi
        seen_paths["$resolved"]=1

        printf '%s\n' "$resolved"
    done <<< "$all_deps"
}

homeboy_export_validation_dependency_paths() {
    local plugin_path="${1:-}"
    local resolved_paths
    resolved_paths=$(homeboy_resolve_validation_dependency_paths "$plugin_path" || true)

    export HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="$resolved_paths"
}
