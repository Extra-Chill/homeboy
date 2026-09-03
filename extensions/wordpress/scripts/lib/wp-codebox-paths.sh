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
    local candidate=""

    for candidate in "${HOMEBOY_WP_CODEBOX_BIN:-}" "${WP_CODEBOX_BIN:-}" "${HOMEBOY_SETTINGS_WP_CODEBOX_BIN:-}"; do
        [ -n "$candidate" ] && break
    done
    if [ -n "$settings_json" ] && [ "$settings_json" != "{}" ]; then
        [ -n "$candidate" ] || candidate=$(printf '%s' "$settings_json" | jq -r '.runtime_bin // empty' 2>/dev/null || true)
        [ -n "$candidate" ] || candidate=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true)
        [ -n "$candidate" ] || candidate=$(printf '%s' "$settings_json" | jq -r '.wpCodeboxBin // empty' 2>/dev/null || true)
    fi
    [ -n "$candidate" ] || candidate="$(homeboy_wp_codebox_machine_override wp_codebox_bin || true)"

    # Explicit configuration is an operator pin. Without one, a managed source
    # checkout owns resolution; an incomplete checkout is repaired, never
    # bypassed through PATH by a possibly incompatible global installation.
    if [ -n "$candidate" ]; then
        if homeboy_wp_codebox_bin_is_runnable "$candidate"; then
            printf '%s\n' "$candidate"
            return 0
        fi
        echo "Error: the configured WP Codebox binary is unavailable or does not satisfy the CLI contract: ${candidate}." >&2
        echo "       Re-run the WordPress extension setup or correct the explicit binary setting." >&2
        return 1
    fi

    # Cache promotion takes this lock before a legacy directory is migrated to
    # the stable source symlink. Never fall through to PATH in that interval.
    if homeboy_wp_codebox_managed_cache_is_updating; then
        echo "Error: the managed WP Codebox cache is updating; retry after the update completes." >&2
        return 1
    fi

    if [ -d "$(homeboy_wp_codebox_managed_install_root)/source" ]; then
        candidate="$(homeboy_wp_codebox_managed_cli_candidates | head -1)"
        if homeboy_wp_codebox_bin_is_runnable "$candidate"; then
            printf '%s\n' "$candidate"
            return 0
        fi
        echo "Error: the managed WP Codebox cache is incomplete; its built CLI entrypoint is missing at ${candidate}." >&2
        echo "       Re-run the WordPress extension setup to rebuild it." >&2
        return 1
    fi

    local candidates=()

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

homeboy_wp_codebox_managed_cache_is_updating() {
    local install_dir
    install_dir="$(homeboy_wp_codebox_managed_install_root)"
    [ -d "${install_dir}/source.update-lock" ]
}

# Machine-scoped override file written by setup (scripts/build/setup.sh) into
# the untracked cache install root. Setup persists the wp_codebox_bin /
# wp_codebox_core_module values it resolved for this machine here instead of
# rewriting the tracked wordpress.json manifest. The resolver consumes this file
# at the same precedence the manifest default used to provide.
homeboy_wp_codebox_machine_override_file() {
    printf '%s\n' "$(homeboy_wp_codebox_managed_install_root)/wp-codebox-overrides.json"
}

homeboy_wp_codebox_machine_override() {
    local field="$1"
    local override_file

    override_file="$(homeboy_wp_codebox_machine_override_file)"
    [ -f "${override_file}" ] || return 1

    jq -r --arg field "${field}" '.[$field] // empty' "${override_file}" 2>/dev/null | head -n 1
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
# Explicit configuration is resolved by homeboy_wp_codebox_resolve_bin. Every
# configured value is an exact pin: a dangling pin fails closed rather than
# silently selecting a managed or PATH runtime.
homeboy_wp_codebox_export_command() {
    local settings_json="${1:-${HOMEBOY_SETTINGS_JSON:-}}"
    local override

    for override in "${HOMEBOY_WP_CODEBOX_BIN:-}" "${WP_CODEBOX_BIN:-}"; do
        [ -n "$override" ] || continue
        if ! homeboy_wp_codebox_bin_is_present "$override"; then
            echo "Error: the configured WP Codebox binary is unavailable or does not satisfy the CLI contract: ${override}." >&2
            echo "       Re-run the WordPress extension setup or correct the explicit binary setting." >&2
            return 1
        fi
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

# Keep shell runners on the same version, capability, and managed-runtime
# identity gate as Node readiness. This validates the exact argv below instead
# of resolving a second candidate after a caller has selected one.
homeboy_wp_codebox_preflight_command() {
    local script_dir
    local selection_module
    local result

    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    # Runtime selection is WordPress-owned and ships in this extension's own
    # lib, so it resolves from the extension payload rather than the shared
    # agent-runtime tree.
    selection_module="${script_dir}/../../lib/wp-codebox-runtime-selection.js"
    if [ ! -f "${selection_module}" ]; then
        printf 'Error: WP Codebox runtime selection module missing at %s; the WordPress extension payload is incomplete.\n' "${selection_module}" >&2
        return 1
    fi
    result="$(node - "${selection_module}" "${HOMEBOY_WP_CODEBOX_COMMAND[@]}" <<'NODE'
const { preflightWpCodeboxCommand } = require(process.argv[2]);
const result = preflightWpCodeboxCommand(process.argv.slice(3));
if (!result.ready) {
    process.stdout.write(`WP Codebox ${result.reason}: required >=${result.required_version}, observed ${result.selected.version || 'unavailable'} at ${result.selected.path || 'no executable'}. Run ${result.remediation}.\n`);
    process.exit(1);
}
NODE
)" || {
        [ -n "${result}" ] && printf '%s\n' "${result}" >&2
        return 1
    }
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
    homeboy_wp_codebox_preflight_command || return 1

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
