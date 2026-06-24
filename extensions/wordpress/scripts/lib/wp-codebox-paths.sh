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
    local bin=""
    local candidate=""
    local candidates=()

    if [ -n "$settings_json" ] && [ "$settings_json" != "{}" ]; then
        bin=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true)
    fi
    if [ -n "$bin" ]; then
        candidates+=("$bin")
    fi
    if [ -n "${HOMEBOY_SETTINGS_WP_CODEBOX_BIN:-}" ]; then
        candidates+=("$HOMEBOY_SETTINGS_WP_CODEBOX_BIN")
    fi
    if [ -n "${HOMEBOY_WP_CODEBOX_BIN:-}" ]; then
        candidates+=("$HOMEBOY_WP_CODEBOX_BIN")
    fi

    while IFS= read -r candidate; do
        [ -n "$candidate" ] && candidates+=("$candidate")
    done < <(type -a -p wp-codebox 2>/dev/null || true)

    while IFS= read -r candidate; do
        [ -n "$candidate" ] && candidates+=("$candidate")
    done < <(homeboy_wp_codebox_global_cli_candidates)

    candidates+=("wp-codebox")

    for candidate in "${candidates[@]}"; do
        [ -n "$candidate" ] || continue
        if homeboy_wp_codebox_bin_is_runnable "$candidate"; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    if ! command -v wp-codebox >/dev/null 2>&1; then
        if [ "$config_label" = "config" ]; then
            echo "ERROR: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN or config wp_codebox_bin" >&2
        else
            echo "Error: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN, settings wp_codebox_bin, or install wp-codebox." >&2
        fi
    else
        echo "Error: wp-codebox was found, but no candidate passed 'wp-codebox --version'. Remove stale wrappers or set HOMEBOY_WP_CODEBOX_BIN to a working binary." >&2
    fi

    return 1
}

homeboy_wp_codebox_global_cli_candidates() {
    local roots=()
    local node_bin=""
    local node_modules=""
    local root=""

    if [ -n "${HOMEBOY_GLOBAL_NODE_MODULE_ROOT:-}" ]; then
        roots+=("$HOMEBOY_GLOBAL_NODE_MODULE_ROOT")
    fi
    if node_bin=$(command -v node 2>/dev/null); then
        node_modules="$(cd "$(dirname "$node_bin")/../lib/node_modules" 2>/dev/null && pwd -P || true)"
        [ -n "$node_modules" ] && roots+=("$node_modules")
    fi

    for root in "${roots[@]}"; do
        printf '%s\n' \
            "${root}/wp-codebox-workspace/packages/cli/dist/index.js" \
            "${root}/@automattic/wp-codebox-cli/dist/index.js" \
            "${root}/wp-codebox-workspace/node_modules/@automattic/wp-codebox-cli/dist/index.js"
    done
}

homeboy_wp_codebox_bin_is_runnable() {
    local bin="$1"

    if [ "$bin" = "wp-codebox" ]; then
        command -v wp-codebox >/dev/null 2>&1 || return 1
    elif [[ "$bin" = /* || "$bin" == ./* || "$bin" == ../* ]]; then
        case "$bin" in
            *.js|*.cjs|*.mjs)
                [ -f "$bin" ] || return 1
                return 0
                ;;
            *)
                [ -x "$bin" ] || return 1
                ;;
        esac
    fi

    "$bin" --version >/dev/null 2>&1
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

homeboy_wp_codebox_run_recipe() {
    local recipe_file="$1"
    local artifacts_dir="$2"
    local output_file="$3"
    local stderr_file="${4:-}"
    local bin="${5:-${WP_CODEBOX_BIN:-}}"
    local had_errexit=0
    local status

    if [ -z "$bin" ]; then
        bin="$(homeboy_wp_codebox_resolve_bin "${HOMEBOY_SETTINGS_JSON:-}")" || return 1
    fi
    homeboy_wp_codebox_set_command "$bin"

    case $- in
        *e*) had_errexit=1 ;;
    esac
    set +e
    if [ -n "$stderr_file" ]; then
        "${HOMEBOY_WP_CODEBOX_COMMAND[@]}" recipe-run --recipe "$recipe_file" --artifacts "$artifacts_dir" --json > "$output_file" 2> "$stderr_file"
    else
        "${HOMEBOY_WP_CODEBOX_COMMAND[@]}" recipe-run --recipe "$recipe_file" --artifacts "$artifacts_dir" --json > "$output_file" 2>&1
    fi
    status=$?
    if [ "$had_errexit" -eq 1 ]; then
        set -e
    fi

    return "$status"
}

homeboy_wp_codebox_recipe_last_stdout() {
    jq -r '(.executions // [])[-1].stdout // empty' "$1" 2>/dev/null
}

homeboy_wp_codebox_recipe_last_stderr() {
    jq -r '(.executions // [])[-1].stderr // empty' "$1" 2>/dev/null
}

homeboy_wp_codebox_recipe_succeeded() {
    jq -e '.success == true' "$1" >/dev/null 2>&1
}

homeboy_wp_codebox_recipe_artifact_directory() {
    jq -r '.artifacts.directory // empty' "$1" 2>/dev/null
}
