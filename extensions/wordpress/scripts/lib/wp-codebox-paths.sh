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
