#!/usr/bin/env bash
set -euo pipefail

# Dependency resolution for WordPress extension validation.
#
# Resolves dependencies from two sources (merged, deduplicated):
#   1. Plugin header "Requires Plugins:" (auto-discovered, zero-config)
#   2. Settings JSON "validation_dependencies" / "depends_on" (manual overrides)
#
# Settings deps take priority — they can be absolute paths or slugs.
# Header deps are resolved through `homeboy component show` by slug.

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

homeboy_resolve_validation_dependency_path() {
    local dependency="${1:-}"

    [ -z "$dependency" ] && return 1

    if [ -d "$dependency" ]; then
        printf '%s\n' "$dependency"
        return 0
    fi

    if command -v homeboy >/dev/null 2>&1; then
        local resolved
        resolved=$(homeboy component show "$dependency" 2>/dev/null | jq -r '.data.entity.local_path // empty' 2>/dev/null || true)
        if [ -n "$resolved" ] && [ -d "$resolved" ]; then
            printf '%s\n' "$resolved"
            return 0
        fi
    fi

    return 1
}

homeboy_resolve_validation_dependency_paths() {
    local plugin_path="${1:-}"

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
