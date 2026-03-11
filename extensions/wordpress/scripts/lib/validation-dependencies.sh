#!/usr/bin/env bash
set -euo pipefail

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
    local raw
    raw=$(homeboy_get_validation_dependencies_raw)

    [ -z "$raw" ] && return 0

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

        printf '%s\n' "$resolved"
    done < <(homeboy_normalize_validation_dependencies "$raw")
}

homeboy_export_validation_dependency_paths() {
    local plugin_path="${1:-}"
    local resolved_paths
    resolved_paths=$(homeboy_resolve_validation_dependency_paths "$plugin_path" || true)

    export HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="$resolved_paths"
}
