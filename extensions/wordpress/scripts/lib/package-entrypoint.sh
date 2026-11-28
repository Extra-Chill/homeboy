#!/usr/bin/env bash

# Read the first matching header within WordPress's 8 KiB header window.
wordpress_package_header() {
    local field="$1"
    LC_ALL=C awk -v field="$field" '
        BEGIN { prefix = "^[[:space:]/*#]*" tolower(field) ":[[:space:]]*" }
        !found && tolower($0) ~ prefix {
            sub(/^[^:]*:[[:space:]]*/, "")
            sub(/[[:space:]]*(\*\/|\?>).*$/, "")
            sub(/[[:space:]]+$/, "")
            print
            found = 1
        }
    '
}

wordpress_package_source_header() {
    head -c 8192 -- "$1" | wordpress_package_header "$2"
}

wordpress_package_is_plugin() {
    [[ -f "$1" && ! -L "$1" && -n "$(wordpress_package_source_header "$1" 'Plugin Name')" ]]
}

# Resolve from the package source root, not the caller's original cwd.
# Status 1 means no headers were found; status 2 means selection is ambiguous.
wordpress_package_entrypoint() {
    local component_id="${1:-}" candidate target
    local plugin_count=0 versioned_count=0 plugin_file="" versioned_file=""

    if [[ -f homeboy.json ]] && command -v jq >/dev/null 2>&1; then
        # Several version targets may point to the same entrypoint. Ignore
        # generated files and targets that aren't root plugin headers.
        target=""
        while IFS= read -r candidate; do
            candidate="${candidate#./}"
            [[ "$candidate" == *.php && "$candidate" != */* ]] || continue
            wordpress_package_is_plugin "$candidate" || continue
            if [[ -n "$target" && "$target" != "$candidate" ]]; then
                target=""
                break
            fi
            target="$candidate"
        done < <(jq -r '.version_targets[]? | .file | strings' homeboy.json 2>/dev/null)
        if [[ -n "$target" ]]; then
            printf '%s\n' "$target"
            return
        fi
        component_id="${component_id:-$(jq -r '.id | strings' homeboy.json 2>/dev/null || true)}"
    fi

    candidate="${component_id:-${PWD##*/}}.php"
    if [[ "$candidate" != */* ]] && wordpress_package_is_plugin "$candidate"; then
        printf '%s\n' "$candidate"
        return
    fi

    for candidate in *.php; do
        wordpress_package_is_plugin "$candidate" || continue
        plugin_count=$((plugin_count + 1))
        plugin_file="$candidate"
        if [[ -n "$(wordpress_package_source_header "$candidate" Version)" ]]; then
            versioned_count=$((versioned_count + 1))
            versioned_file="$candidate"
        fi
    done

    if [[ "$plugin_count" -eq 1 ]]; then
        printf '%s\n' "$plugin_file"
    elif [[ "$versioned_count" -eq 1 ]]; then
        printf '%s\n' "$versioned_file"
    elif [[ "$plugin_count" -gt 1 ]]; then
        echo "Error: ambiguous WordPress plugin entrypoint; declare its file in homeboy.json version_targets or use a component ID matching its filename" >&2
        return 2
    elif [[ -f style.css && -n "$(wordpress_package_source_header style.css 'Theme Name')" ]]; then
        printf '%s\n' style.css
    else
        return 1
    fi
}
