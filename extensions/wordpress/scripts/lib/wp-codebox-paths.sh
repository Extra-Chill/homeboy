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
        bin=$(printf '%s' "$settings_json" | jq -r '.runtime_bin // empty' 2>/dev/null || true)
    fi
    if [ -n "$bin" ]; then
        candidates+=("$bin")
    fi

    while IFS= read -r candidate; do
        [ -n "$candidate" ] && candidates+=("$candidate")
    done < <(homeboy_wp_codebox_managed_cli_candidates)

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

    if homeboy_wp_codebox_managed_cache_is_incomplete; then
        local managed_cli
        managed_cli="$(homeboy_wp_codebox_managed_cli_candidates | head -1)"
        echo "Error: the managed WP Codebox cache is incomplete; its built CLI entrypoint is missing at ${managed_cli}." >&2
        echo "       Re-run the WordPress extension setup to rebuild it, or set HOMEBOY_WP_CODEBOX_BIN / settings wp_codebox_bin to a working CLI." >&2
    elif ! command -v wp-codebox >/dev/null 2>&1; then
        if [ "$config_label" = "config" ]; then
            echo "ERROR: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN or config wp_codebox_bin" >&2
        else
            echo "Error: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN, settings wp_codebox_bin, or install wp-codebox." >&2
        fi
    else
        echo "Error: wp-codebox was found, but no candidate passed 'wp-codebox commands'. Remove stale wrappers or set HOMEBOY_WP_CODEBOX_BIN to a working binary." >&2
    fi

    return 1
}

homeboy_wp_codebox_global_cli_candidates() {
    local roots=()
    local node_bin=""
    local node_modules=""
    local npm_root=""
    local root=""
    local seen_roots=""

    if [ -n "${HOMEBOY_GLOBAL_NODE_MODULE_ROOT:-}" ]; then
        roots+=("$HOMEBOY_GLOBAL_NODE_MODULE_ROOT")
    fi
    if node_bin=$(command -v node 2>/dev/null); then
        node_modules="$(cd "$(dirname "$node_bin")/../lib/node_modules" 2>/dev/null && pwd -P || true)"
        [ -n "$node_modules" ] && roots+=("$node_modules")
    fi
    if npm_root=$(npm root -g 2>/dev/null); then
        [ -n "$npm_root" ] && roots+=("$npm_root")
    fi

    for root in "${roots[@]}"; do
        [ -n "$root" ] || continue
        case "\n${seen_roots}\n" in
            *"\n${root}\n"*) continue ;;
        esac
        seen_roots="${seen_roots}\n${root}"
        printf '%s\n' \
            "${root}/wp-codebox-workspace/packages/cli/dist/index.js" \
            "${root}/@automattic/wp-codebox-cli/dist/index.js" \
            "${root}/wp-codebox-workspace/node_modules/@automattic/wp-codebox-cli/dist/index.js"
    done
}

homeboy_wp_codebox_managed_install_root() {
    printf '%s\n' "${HOMEBOY_WP_CODEBOX_INSTALL_DIR:-${HOME}/.cache/homeboy/wp-codebox}"
}

homeboy_wp_codebox_managed_cli_candidates() {
    local install_dir
    install_dir="$(homeboy_wp_codebox_managed_install_root)"

    printf '%s\n' "${install_dir}/source/packages/cli/dist/index.js"
}

# True when the managed source checkout exists but its built CLI entrypoint does
# not. That is an incomplete cache: the clone succeeded and the build did not,
# or the build output was pruned. Callers refresh it rather than treating the
# checkout as a satisfied install.
homeboy_wp_codebox_managed_cache_is_incomplete() {
    local install_dir
    local repo_dir
    install_dir="$(homeboy_wp_codebox_managed_install_root)"
    repo_dir="${install_dir}/source"

    [ -d "${repo_dir}" ] || return 1
    [ ! -f "${repo_dir}/packages/cli/dist/index.js" ]
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

    "$bin" commands >/dev/null 2>&1
}

# Presence check for a binary a caller pinned explicitly. An ambient candidate
# additionally has to answer `commands`, because an install can leave a wrapper
# on PATH after its target is gone. A pinned binary is not ambient: the caller
# named it, so only require that it is actually there to run.
homeboy_wp_codebox_bin_is_present() {
    local bin="$1"

    case "$bin" in
        *.js|*.cjs|*.mjs)
            [ -f "$bin" ]
            ;;
        /*|./*|../*)
            [ -x "$bin" ]
            ;;
        *)
            command -v "$bin" >/dev/null 2>&1
            ;;
    esac
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

# Serialize the resolved invocation as a JSON argv array so non-shell consumers
# (the PHPUnit adapter) inherit this resolver's precedence and `node` prefixing
# instead of maintaining a second candidate list.
homeboy_wp_codebox_command_json() {
    local element
    local encoded
    local out=""

    for element in "${HOMEBOY_WP_CODEBOX_COMMAND[@]}"; do
        encoded="$(printf '%s' "$element" | jq -R -s '.')"
        if [ -z "$out" ]; then
            out="$encoded"
        else
            out="${out},${encoded}"
        fi
    done

    printf '[%s]\n' "$out"
}

homeboy_wp_codebox_publish_command() {
    HOMEBOY_WP_CODEBOX_COMMAND_JSON="$(homeboy_wp_codebox_command_json)"
    export HOMEBOY_WP_CODEBOX_COMMAND_JSON
}

# Resolve once and export the argv contract for child processes.
#
# An explicit override that is present wins outright. The general resolver
# deliberately ranks the managed cache ahead of the environment so a stale
# exported path cannot shadow a freshly built cache, but a caller that pins a
# binary — a test fixture, or an operator pointing at a local build — means it.
# An override pointing at something that is not there is skipped rather than
# trusted, so a dangling pin still falls through to full resolution and its
# diagnostics instead of reaching the runtime.
homeboy_wp_codebox_export_command() {
    local settings_json="${1:-${HOMEBOY_SETTINGS_JSON:-}}"
    local override

    for override in "${HOMEBOY_WP_CODEBOX_BIN:-}" "${WP_CODEBOX_BIN:-}"; do
        [ -n "$override" ] || continue
        homeboy_wp_codebox_bin_is_present "$override" || continue
        homeboy_wp_codebox_set_command "$override"
        homeboy_wp_codebox_publish_command
        return 0
    done

    homeboy_wp_codebox_resolve_command "$settings_json" >/dev/null || return 1
    homeboy_wp_codebox_publish_command
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
