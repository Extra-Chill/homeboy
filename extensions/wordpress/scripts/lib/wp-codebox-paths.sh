#!/usr/bin/env bash

# Resolve a host path before mounting it into WP Codebox. WordPress Playground's
# PHP-WASM VFS does not reliably expose symlinked mount roots unless the host
# path is resolved first.
homeboy_wp_codebox_resolve_mount_path() {
    local host_path="$1"

    if [ -d "$host_path" ]; then
        (cd "$host_path" && pwd -P)
        return
    fi

    local parent
    local basename
    parent="$(dirname "$host_path")"
    basename="$(basename "$host_path")"

    if [ -d "$parent" ]; then
        printf '%s/%s\n' "$(cd "$parent" && pwd -P)" "$basename"
        return
    fi

    printf '%s\n' "$host_path"
}

homeboy_wp_codebox_resolve_host_path() {
    local root="$1"
    local ref="$2"

    if [[ "$ref" = /* ]]; then
        printf '%s\n' "$ref"
    else
        printf '%s/%s\n' "${root%/}" "$ref"
    fi
}

homeboy_wp_codebox_component_relative_path() {
    local host_path="$1"

    if [[ "$host_path" = "$PLUGIN_PATH"/* ]]; then
        printf '%s\n' "${host_path#"$PLUGIN_PATH/"}"
    else
        printf '%s\n' "$host_path"
    fi
}

homeboy_wp_codebox_resolve_bin() {
    local settings_json="${1:-${HOMEBOY_SETTINGS_JSON:-}}"
    local config_label="${2:-settings}"
    local bin="${HOMEBOY_WP_CODEBOX_BIN:-}"

    if [ -z "$bin" ] && [ -n "$settings_json" ] && [ "$settings_json" != "{}" ]; then
        bin=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true)
    fi

    bin="${bin:-wp-codebox}"
    if [ "$bin" = "wp-codebox" ] && ! command -v wp-codebox >/dev/null 2>&1; then
        if [ "$config_label" = "config" ]; then
            echo "ERROR: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN or config wp_codebox_bin" >&2
        else
            echo "Error: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN, settings wp_codebox_bin, or install wp-codebox." >&2
        fi
        return 1
    fi

    printf '%s\n' "$bin"
}

homeboy_wp_codebox_set_command() {
    local bin="$1"

    HOMEBOY_WP_CODEBOX_COMMAND=("$bin")
    case "$bin" in
        *.js|*.cjs|*.mjs)
            HOMEBOY_WP_CODEBOX_COMMAND=(node "$bin")
            ;;
    esac
}

homeboy_wp_codebox_resolve_command() {
    local settings_json="${1:-${HOMEBOY_SETTINGS_JSON:-}}"
    local bin

    bin="$(homeboy_wp_codebox_resolve_bin "$settings_json")" || return 1
    homeboy_wp_codebox_set_command "$bin"
    printf '%s\n' "$bin"
}

homeboy_wp_codebox_resolved_bin_path() {
    local bin="$1"
    local resolved_bin

    if resolved_bin=$(command -v "$bin" 2>/dev/null); then
        printf '%s\n' "$resolved_bin"
        return 0
    fi

    printf '%s\n' "$bin"
}
