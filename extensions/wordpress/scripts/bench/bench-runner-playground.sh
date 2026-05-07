#!/usr/bin/env bash
set -euo pipefail

# Playground bench runner for WordPress Homeboy extension.
#
# Boots a WordPress Playground instance (PHP-WASM + embedded SQLite),
# mounts the component under test, runs every workload in
# `tests/bench/*.php`, and emits a `BenchResults` JSON envelope at
# $HOMEBOY_BENCH_RESULTS_FILE matching the contract in homeboy core's
# `extension/bench/parsing.rs`.
#
# HOW IT WORKS
#
# 1. Fills in the static PHP template (playground-bench-runner.php) with
#    the plugin slug + dependency mount paths via sed substitution.
#    The template:
#    - Requires the shared playground-bootstrap.php lib so the boot path
#      is byte-identical to the test runner's.
#    - Calls the four shared stage executors (boot, install, load_deps,
#      load_component).
#    - Discovers tests/bench/*.php workloads.
#    - For each workload, runs `bench_main()` $HOMEBOY_BENCH_ITERATIONS
#      times and computes p50/p95/p99/mean/min/max in PHP.
#    - Writes the BenchResults envelope to .pg-bench-results.json.
#
# 2. Mounts host directories into Playground's VFS (same shape as test
#    runner): plugin under test, dependencies, extension dir, and any
#    custom db.php drop-in.
#
# 3. Runs the filled template via `wp-playground-cli php`.
#
# 4. Reads the host-visible .pg-bench-results.json (written through the
#    mount) and copies it to $HOMEBOY_BENCH_RESULTS_FILE for homeboy core.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
BENCH_HELPER_SH="${HOMEBOY_RUNTIME_BENCH_HELPER_SH:-${HOME}/.homeboy/runtime/bench-helper.sh}"
BENCH_HELPER_PHP_HOST="${HOMEBOY_RUNTIME_BENCH_HELPER_PHP:-${HOME}/.homeboy/runtime/bench-helper.php}"
BENCH_HELPER_PHP_GUEST="/homeboy-runtime/bench-helper.php"
BENCH_BROWSER_TARGET_HELPER="${SCRIPT_DIR}/browser-target.sh"
PHP_PREFLIGHT_HELPER="${SCRIPT_DIR}/../lib/php-preflight.sh"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
PLAYGROUND_PATHS_HELPER="${SCRIPT_DIR}/../lib/playground-paths.sh"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH
# shellcheck source=../lib/php-preflight.sh
if [ -f "$PHP_PREFLIGHT_HELPER" ]; then
    source "$PHP_PREFLIGHT_HELPER"
fi
# shellcheck source=../lib/validation-dependencies.sh
if [ -f "$DEPENDENCY_HELPER" ]; then
    source "$DEPENDENCY_HELPER"
fi
# shellcheck source=../lib/playground-paths.sh
source "$PLAYGROUND_PATHS_HELPER"
# shellcheck source=/dev/null
if [ -n "$FAILURE_TRAP_HELPER" ] && [ -f "$FAILURE_TRAP_HELPER" ]; then
    source "$FAILURE_TRAP_HELPER"
    homeboy_init_failure_trap
else
    FAILED_STEP=""
    FAILURE_OUTPUT=""
fi
# shellcheck source=/dev/null
if [ -f "$BENCH_HELPER_SH" ]; then
    source "$BENCH_HELPER_SH"
else
    echo "ERROR: Homeboy bench helper not found at ${BENCH_HELPER_SH}" >&2
    exit 2
fi
if [ ! -f "$BENCH_HELPER_PHP_HOST" ]; then
    echo "ERROR: Homeboy PHP bench helper not found at ${BENCH_HELPER_PHP_HOST}" >&2
    exit 2
fi
# shellcheck source=browser-target.sh
source "$BENCH_BROWSER_TARGET_HELPER"

# PLUGIN_SLUG is the wp-content/plugins/ path segment Playground uses to
# mount the component-under-test. The historical default was
# `basename($PLUGIN_PATH)`, which works for canonical plugin checkouts
# whose directory name equals the plugin slug. It breaks for git-worktree
# checkouts (`<repo>@<branch-slug>` convention) and for any workspace where
# the on-disk directory name diverges from the canonical slug — the wrong
# wp-content/plugins/ path makes plugin-internal asset URLs, intra-plugin
# require_once paths, and `Requires Plugins:` resolution all fail.
#
# When homeboy core tells us the canonical component id (HOMEBOY_COMPONENT_ID),
# use it. Fall back to basename only when no id is set (direct invocation
# from a CWD that isn't a registered component).
if [ -n "${COMPONENT_ID:-}" ]; then
    PLUGIN_SLUG="$COMPONENT_ID"
else
    PLUGIN_SLUG="$(basename "$PLUGIN_PATH")"
    COMPONENT_ID="$PLUGIN_SLUG"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [bench:playground] Extension path: $EXTENSION_PATH"
    echo "DEBUG: [bench:playground] Plugin path: $PLUGIN_PATH"
    echo "DEBUG: [bench:playground] Component ID: $COMPONENT_ID"
fi

PLAYGROUND_CLI="${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"
if [ ! -f "$PLAYGROUND_CLI" ]; then
    echo "Error: @wp-playground/cli not found at $PLAYGROUND_CLI" >&2
    echo "" >&2
    echo "Install it with: cd ${EXTENSION_PATH} && npm install" >&2
    FAILED_STEP="Playground CLI setup"
    exit 1
fi

BENCH_WORKLOADS_FILTER_PROVIDED=0
if [ -n "${HOMEBOY_BENCH_WORKLOADS:-}" ]; then
    BENCH_WORKLOADS_FILTER_PROVIDED=1
elif [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    if printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -e 'has("bench_workloads") and (.bench_workloads != null)' >/dev/null 2>&1; then
        BENCH_WORKLOADS_FILTER_PROVIDED=1
    fi
fi

PLAYGROUND_WORKLOADS_PROVIDED=0
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    if printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -e 'has("playground_workloads") and (.playground_workloads != null) and (.playground_workloads != [])' >/dev/null 2>&1; then
        PLAYGROUND_WORKLOADS_PROVIDED=1
    fi
fi

BENCH_DIR="${PLUGIN_PATH}/tests/bench"
if [ ! -d "$BENCH_DIR" ] && [ -z "${HOMEBOY_BENCH_EXTRA_WORKLOADS:-}" ] && [ "$BENCH_WORKLOADS_FILTER_PROVIDED" != "1" ] && [ "$PLAYGROUND_WORKLOADS_PROVIDED" != "1" ]; then
    echo ""
    echo "⚠ No tests/bench directory found at ${BENCH_DIR}"
    echo "  Skipping bench run — nothing to measure."
    echo ""
    # Emit an empty-but-valid BenchResults so homeboy core's parser
    # doesn't see a missing file and treat the run as a crash.
    if [ -n "${HOMEBOY_BENCH_RESULTS_FILE:-}" ]; then
        homeboy_write_empty_bench_results "$COMPONENT_ID" 0 "${HOMEBOY_BENCH_RESULTS_FILE}"
    fi
    exit 0
fi

if type homeboy_php_preflight &>/dev/null; then
    homeboy_php_preflight "$PLUGIN_PATH"
fi

# Export env vars the playground-runner downstream of us reads.
if [ -n "${COMPONENT_ID:-}" ]; then
    export HOMEBOY_COMPONENT_ID="$COMPONENT_ID"
    export HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH"
    export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"
fi

if type homeboy_export_validation_dependency_paths &>/dev/null; then
    homeboy_export_validation_dependency_paths "$PLUGIN_PATH"
fi
DEPENDENCY_PATHS="${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}"

# Extract `wp_config_defines` from the merged settings JSON. The component
# declares its own additional wp-config defines under
# `extensions.wordpress.settings.wp_config_defines`; homeboy core merges
# them into HOMEBOY_SETTINGS_JSON and the runner appends them to
# wp-tests-config.php during pg_run_boot_stage().
#
# Default to an empty object when unset/malformed — pg_run_boot_stage
# treats {} as "no extra defines" (no-op for components that don't need
# the seam, which is the canonical case).
WP_CONFIG_DEFINES_JSON="{}"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.wp_config_defines // {}' 2>/dev/null || echo "{}")
    if [ -n "$extracted" ]; then
        WP_CONFIG_DEFINES_JSON="$extracted"
    fi
fi

# Extract `bench_env` from the merged settings JSON. Components declare
# host-shell env vars that should propagate into Playground PHP-WASM under
# `extensions.wordpress.settings.bench_env` in their homeboy.json:
#
#   { "bench_env": { "BENCH_CORPUS_SIZE": "1000", "BENCH_SEED": "42" } }
#
# Workloads then read them via `getenv('BENCH_CORPUS_SIZE')` as if they
# had been set on the host shell. Without this seam, host env vars don't
# cross the wp-playground-cli sandbox boundary — workloads' getenv() calls
# return false regardless of what the parent shell set.
#
# Default `{}` is the no-op case for components that don't need the seam.
BENCH_ENV_JSON="{}"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.bench_env // {}' 2>/dev/null || echo "{}")
    if [ -n "$extracted" ]; then
        BENCH_ENV_JSON="$extracted"
    fi
fi
BENCH_ENV_JSON_B64=$(printf '%s' "$BENCH_ENV_JSON" | base64 | tr -d '\n')

# Extract `bench_workloads` from the merged settings JSON. Components can
# restrict a bench run to specific workload IDs under
# `extensions.wordpress.settings.bench_workloads` in their homeboy.json:
#
#   { "bench_workloads": ["boot-timing", "read-heavy"] }
#   { "bench_workloads": "boot-timing,read-heavy" }
#
# Direct runner invocations can use HOMEBOY_BENCH_WORKLOADS with the same
# comma-separated string shape. The PHP runner normalizes both forms after
# discovery so omitted workloads never enter the BenchResults envelope.
BENCH_WORKLOADS_JSON="null"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c 'if has("bench_workloads") then .bench_workloads else null end' 2>/dev/null || echo "null")
    if [ -n "$extracted" ] && [ "$extracted" != "null" ]; then
        BENCH_WORKLOADS_JSON="$extracted"
    fi
fi

# Extract `playground_workloads` from the merged settings JSON. These are
# config-declared workloads that run after the shared Playground bootstrap,
# blueprint application, dependency mounts, and component load. They emit the
# same BenchResults scenario shape as PHP files under tests/bench/.
PLAYGROUND_WORKLOADS_JSON="[]"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.playground_workloads // []' 2>/dev/null || echo "[]")
    if [ -n "$extracted" ] && [ "$extracted" != "null" ]; then
        PLAYGROUND_WORKLOADS_JSON="$extracted"
    fi
fi
if [ "$BENCH_WORKLOADS_JSON" = "null" ] && [ -n "${HOMEBOY_BENCH_WORKLOADS:-}" ]; then
    BENCH_WORKLOADS_JSON=$(jq -Rn --arg value "$HOMEBOY_BENCH_WORKLOADS" '$value')
fi

# Extract `bench_site_mode` from the merged settings JSON. Default `fresh`
# preserves the historical wp-phpunit install path. `installed` mounts a
# persisted /wordpress tree from HOMEBOY_BENCH_SHARED_STATE, lets Playground
# prepare that tree on the first run, and has the PHP runner boot wp-load.php
# instead of re-running wp-phpunit install.php on warm runs.
BENCH_SITE_MODE="${HOMEBOY_BENCH_SITE_MODE:-fresh}"
if [ -z "${HOMEBOY_BENCH_SITE_MODE:-}" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.bench_site_mode // "fresh"' 2>/dev/null || echo "fresh")
    if [ -n "$extracted" ] && [ "$extracted" != "null" ]; then
        BENCH_SITE_MODE="$extracted"
    fi
fi
case "$BENCH_SITE_MODE" in
    fresh|installed)
        ;;
    *)
        echo "Error: bench_site_mode must be 'fresh' or 'installed' (got '$BENCH_SITE_MODE')" >&2
        FAILED_STEP="Bench site mode setup"
        exit 1
        ;;
esac

# Optional Playground blueprint JSON. Non-empty objects are written to a host
# tempfile and passed to wp-playground-cli --blueprint so cold-boot scenarios
# can measure plugin/theme installation as part of Playground bootstrap.
PLAYGROUND_BLUEPRINT_JSON="{}"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.playground_blueprint // {}' 2>/dev/null || echo "{}")
    if [ -n "$extracted" ] && [ "$extracted" != "null" ]; then
        PLAYGROUND_BLUEPRINT_JSON="$extracted"
    fi
fi

# Optional direct pass-through for Playground's own install-mode flag. Most
# components should use bench_site_mode; this is a narrow escape hatch for
# runner-level experiments without teaching homeboy core about Playground.
PLAYGROUND_WORDPRESS_INSTALL_MODE=""
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.playground_wordpress_install_mode // empty' 2>/dev/null || true)
    if [ -n "$extracted" ] && [ "$extracted" != "null" ]; then
        PLAYGROUND_WORDPRESS_INSTALL_MODE="$extracted"
    fi
fi

PLAYGROUND_WORDPRESS_VERSION="6.9"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.playground_wordpress_version // empty' 2>/dev/null || true)
    if [ -n "$extracted" ] && [ "$extracted" != "null" ]; then
        PLAYGROUND_WORDPRESS_VERSION="$extracted"
    fi
fi

ITERATIONS="${HOMEBOY_BENCH_ITERATIONS:-10}"

# ---------------------------------------------------------------------------
# Build VFS mount args (mirrors test-runner-playground.sh shape)
# ---------------------------------------------------------------------------
MOUNT_BEFORE_INSTALL_ARGS=()
MOUNT_ARGS=()
MOUNT_ARGS+=("--mount" "${PLUGIN_PATH}:/wordpress/wp-content/plugins/${PLUGIN_SLUG}")

if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
        MOUNT_ARGS+=("--mount" "${dep_path}:/wordpress/wp-content/plugins/${dep_slug}")
    done <<< "$DEPENDENCY_PATHS"
fi

PLUGIN_DB_PHP="${PLUGIN_PATH}/db.php"
if [ -f "$PLUGIN_DB_PHP" ]; then
    MOUNT_ARGS+=("--mount" "${PLUGIN_DB_PHP}:/wordpress/wp-content/db.php")
fi

EXTENSION_MOUNT_PATH="$(homeboy_playground_resolve_mount_path "$EXTENSION_PATH")"
MOUNT_ARGS+=("--mount" "${EXTENSION_MOUNT_PATH}:/homeboy-extension")
MOUNT_ARGS+=("--mount" "${BENCH_HELPER_PHP_HOST}:${BENCH_HELPER_PHP_GUEST}")

# Rig-private workloads are host paths supplied by homeboy core through a
# PATH-style list. Mount each file into Playground and let the PHP runner tag
# them as `source: rig` in the BenchResults envelope.
EXTRA_WORKLOADS_LIST=""
if [ -n "${HOMEBOY_BENCH_EXTRA_WORKLOADS:-}" ]; then
    EXTRA_WORKLOAD_INDEX=0
    IFS=':' read -r -a EXTRA_WORKLOAD_HOSTS <<< "${HOMEBOY_BENCH_EXTRA_WORKLOADS}"
    for extra_workload_host in "${EXTRA_WORKLOAD_HOSTS[@]}"; do
        [ -n "$extra_workload_host" ] || continue
        if [ ! -f "$extra_workload_host" ]; then
            echo "Error: rig bench workload not found: $extra_workload_host" >&2
            FAILED_STEP="Rig bench workload setup"
            exit 1
        fi
        extra_workload_guest="/bench-extra-workloads/${EXTRA_WORKLOAD_INDEX}-$(basename "$extra_workload_host")"
        MOUNT_ARGS+=("--mount" "${extra_workload_host}:${extra_workload_guest}")
        if [ -n "$EXTRA_WORKLOADS_LIST" ]; then
            EXTRA_WORKLOADS_LIST+=":"
        fi
        EXTRA_WORKLOADS_LIST+="$extra_workload_guest"
        EXTRA_WORKLOAD_INDEX=$((EXTRA_WORKLOAD_INDEX + 1))
    done
fi

# Shared-state mount: when homeboy core passes HOMEBOY_BENCH_SHARED_STATE,
# we mount the host directory into Playground's VFS at a stable path
# (/bench-shared-state) and expose that path to the workload via the
# template. This is the on-disk substrate for concurrent-writer and
# crash-recovery workloads — see homeboy#1508.
SHARED_STATE_HOST="${HOMEBOY_BENCH_SHARED_STATE:-}"
SHARED_STATE_GUEST=""
if [ -n "$SHARED_STATE_HOST" ]; then
    if [ ! -d "$SHARED_STATE_HOST" ]; then
        # Homeboy core creates this dir before invocation, but if a caller
        # invokes the dispatcher directly we still want to be helpful.
        mkdir -p "$SHARED_STATE_HOST"
    fi
    SHARED_STATE_GUEST="/bench-shared-state"
    MOUNT_ARGS+=("--mount" "${SHARED_STATE_HOST}:${SHARED_STATE_GUEST}")
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: [bench:playground] Shared state: ${SHARED_STATE_HOST} → ${SHARED_STATE_GUEST}"
    fi
fi

if [ "$BENCH_SITE_MODE" = "installed" ]; then
    if [ -z "$SHARED_STATE_HOST" ]; then
        echo "Error: bench_site_mode=installed requires HOMEBOY_BENCH_SHARED_STATE so /wordpress can persist across runs." >&2
        FAILED_STEP="Installed-site bench setup"
        exit 1
    fi
    SITE_STATE_HOST="${SHARED_STATE_HOST}/wordpress"
    mkdir -p "$SITE_STATE_HOST"
    MOUNT_BEFORE_INSTALL_ARGS+=("--mount-before-install" "${SITE_STATE_HOST}:/wordpress")
    if [ -z "$PLAYGROUND_WORDPRESS_INSTALL_MODE" ]; then
        if [ -f "${SITE_STATE_HOST}/wp-load.php" ]; then
            PLAYGROUND_WORDPRESS_INSTALL_MODE="install-from-existing-files-if-needed"
        else
            PLAYGROUND_WORDPRESS_INSTALL_MODE="download-and-install"
        fi
    fi
else
    if [ -z "$PLAYGROUND_WORDPRESS_INSTALL_MODE" ]; then
        PLAYGROUND_WORDPRESS_INSTALL_MODE="download-and-install"
    fi
fi

if ! homeboy_wordpress_emit_browser_target "${HOMEBOY_SETTINGS_JSON:-{}}" "$SHARED_STATE_HOST" "$COMPONENT_ID" "$PLUGIN_SLUG" "$BENCH_SITE_MODE"; then
    FAILED_STEP="Browser bench target setup"
    exit 1
fi

BLUEPRINT_TMPFILE=""
BLUEPRINT_ARGS=()
if printf '%s' "$PLAYGROUND_BLUEPRINT_JSON" | jq -e 'type == "object" and length > 0' >/dev/null 2>&1; then
    BLUEPRINT_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/pg-blueprint.XXXXXX.json")
    printf '%s' "$PLAYGROUND_BLUEPRINT_JSON" > "$BLUEPRINT_TMPFILE"
    BLUEPRINT_ARGS+=("--blueprint" "$BLUEPRINT_TMPFILE")
fi

INSTANCE_ID="${HOMEBOY_BENCH_INSTANCE_ID:-0}"
CONCURRENCY="${HOMEBOY_BENCH_CONCURRENCY:-1}"
LIST_ONLY="${HOMEBOY_BENCH_LIST_ONLY:-0}"
if [ "$LIST_ONLY" = "1" ]; then
    LIST_ONLY_PHP="true"
else
    LIST_ONLY_PHP="false"
fi

PLAYGROUND_DEP_MOUNTS=""
if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
        if [ -n "$PLAYGROUND_DEP_MOUNTS" ]; then
            PLAYGROUND_DEP_MOUNTS+="\\n"
        fi
        PLAYGROUND_DEP_MOUNTS+="/wordpress/wp-content/plugins/${dep_slug}"
    done <<< "$DEPENDENCY_PATHS"
fi

PLAYGROUND_BLUEPRINT_PLUGIN_SLUGS=""
if printf '%s' "$PLAYGROUND_BLUEPRINT_JSON" | jq -e 'type == "object" and length > 0' >/dev/null 2>&1; then
    while IFS= read -r plugin_slug; do
        [ -n "$plugin_slug" ] || continue
        if [ -n "$PLAYGROUND_BLUEPRINT_PLUGIN_SLUGS" ]; then
            PLAYGROUND_BLUEPRINT_PLUGIN_SLUGS+="\\n"
        fi
        PLAYGROUND_BLUEPRINT_PLUGIN_SLUGS+="$plugin_slug"
    done < <(printf '%s' "$PLAYGROUND_BLUEPRINT_JSON" | jq -r '.steps[]? | select(.step == "installPlugin") | .options.targetFolderName // empty' 2>/dev/null || true)
fi

# Two host-visible files written through the plugin mount:
# - .pg-bench-results.json: the BenchResults envelope (the deliverable).
# - .pg-bench-result.txt:   pg_log / pg_stage_* structured stage log,
#                           used to surface bootstrap failures the same way
#                           test runner does.
#
# When homeboy core spawns N parallel instances (concurrency > 1) the same
# plugin path is mounted into N concurrent Playground processes — without
# per-instance namespacing the workers would race on these two files. Use
# the instance suffix when CONCURRENCY > 1; keep the legacy filename for
# single-instance so direct invocations and existing diagnostics paths
# stay unchanged.
if [ "${CONCURRENCY}" != "1" ]; then
    RESULT_SUFFIX=".i${INSTANCE_ID}"
else
    RESULT_SUFFIX=""
fi
RESULT_JSON="${PLUGIN_PATH}/.pg-bench-results${RESULT_SUFFIX}.json"
RESULT_LOG="${PLUGIN_PATH}/.pg-bench-result${RESULT_SUFFIX}.txt"
rm -f "$RESULT_JSON" "$RESULT_LOG"

TEMPLATE="${SCRIPT_DIR}/playground-bench-runner.php"
if [ ! -f "$TEMPLATE" ]; then
    echo "Error: playground-bench-runner.php template not found at $TEMPLATE" >&2
    FAILED_STEP="Playground bench setup"
    exit 1
fi

WRAPPER_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/pg-bench-runner.XXXXXX")
# Use ASCII SOH (\x01) as the sed delimiter for the JSON substitution so
# embedded `|` / `/` / `,` in user-supplied wp_config_defines values don't
# need escaping. The other placeholders use `|` (their values never contain
# a pipe in practice — slugs, integers, and POSIX paths only).
WP_CONFIG_DEFINES_DELIM=$(printf '\1')

# Escape sed `s` replacement metacharacters in JSON values before
# substituting them into the PHP runner template. GNU sed processes the
# replacement string by treating `&` as a backreference to the matched
# pattern and `\X` as an escape sequence — so an unescaped `&` in JSON
# (e.g. an ampersand inside a string) gets replaced with the placeholder
# itself, and `\"` collapses to `"`, silently corrupting the JSON. The
# decode in playground-bench-runner.php then fails and ALL declared
# bench_env / wp_config_defines entries drop on the floor.
#
# Only `\` and `&` need escaping here: the SOH delimiter cannot appear in
# JSON content, and the only `\X` sequences sed treats specially are
# `\&`, `\\`, the delimiter, and `\1`-`\9` — which all share the `\`
# escape, so escaping `\` covers them.
sed_escape_replacement() {
    printf '%s' "$1" | sed -e 's/[\\&]/\\&/g'
}
WP_CONFIG_DEFINES_JSON_ESC=$(sed_escape_replacement "$WP_CONFIG_DEFINES_JSON")
BENCH_WORKLOADS_JSON_ESC=$(sed_escape_replacement "$BENCH_WORKLOADS_JSON")
PLAYGROUND_WORKLOADS_JSON_ESC=$(sed_escape_replacement "$PLAYGROUND_WORKLOADS_JSON")
EXTRA_WORKLOADS_LIST_ESC=$(sed_escape_replacement "$EXTRA_WORKLOADS_LIST")

sed \
    -e "s|{{PLUGIN_SLUG}}|${PLUGIN_SLUG}|g" \
    -e "s|{{COMPONENT_ID}}|${COMPONENT_ID}|g" \
    -e "s|{{ITERATIONS}}|${ITERATIONS}|g" \
    -e "s|{{PLAYGROUND_DEP_MOUNTS}}|${PLAYGROUND_DEP_MOUNTS}|g" \
    -e "s|{{PLAYGROUND_BLUEPRINT_PLUGIN_SLUGS}}|${PLAYGROUND_BLUEPRINT_PLUGIN_SLUGS}|g" \
    -e "s|{{SHARED_STATE_PATH}}|${SHARED_STATE_GUEST}|g" \
    -e "s|{{INSTANCE_ID}}|${INSTANCE_ID}|g" \
    -e "s|{{CONCURRENCY}}|${CONCURRENCY}|g" \
    -e "s|{{LIST_ONLY}}|${LIST_ONLY_PHP}|g" \
    -e "s|{{RESULT_SUFFIX}}|${RESULT_SUFFIX}|g" \
    -e "s|{{BENCH_HELPER_PHP}}|${BENCH_HELPER_PHP_GUEST}|g" \
    -e "s|{{BENCH_SITE_MODE}}|${BENCH_SITE_MODE}|g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{WP_CONFIG_DEFINES_JSON}}${WP_CONFIG_DEFINES_DELIM}${WP_CONFIG_DEFINES_JSON_ESC}${WP_CONFIG_DEFINES_DELIM}g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{BENCH_ENV_JSON_B64}}${WP_CONFIG_DEFINES_DELIM}${BENCH_ENV_JSON_B64}${WP_CONFIG_DEFINES_DELIM}g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{BENCH_WORKLOADS_JSON}}${WP_CONFIG_DEFINES_DELIM}${BENCH_WORKLOADS_JSON_ESC}${WP_CONFIG_DEFINES_DELIM}g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{PLAYGROUND_WORKLOADS_JSON}}${WP_CONFIG_DEFINES_DELIM}${PLAYGROUND_WORKLOADS_JSON_ESC}${WP_CONFIG_DEFINES_DELIM}g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{EXTRA_WORKLOADS_LIST}}${WP_CONFIG_DEFINES_DELIM}${EXTRA_WORKLOADS_LIST_ESC}${WP_CONFIG_DEFINES_DELIM}g" \
    "$TEMPLATE" > "$WRAPPER_TMPFILE"

echo "Running performance benchmarks via WordPress Playground..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Iterations: ${ITERATIONS}"
echo "  Backend: playground (PHP-WASM + SQLite)"
echo "  Site mode: ${BENCH_SITE_MODE}"
if [ "$LIST_ONLY" = "1" ]; then
    echo "  Mode: list only"
fi
if [ -n "$SHARED_STATE_GUEST" ]; then
    echo "  Shared state: ${SHARED_STATE_HOST} (instance ${INSTANCE_ID}/${CONCURRENCY})"
fi
if [ -n "$BLUEPRINT_TMPFILE" ]; then
    echo "  Playground blueprint: enabled"
fi
if [ "$PLAYGROUND_WORKLOADS_PROVIDED" = "1" ]; then
    echo "  Configured Playground workloads: enabled"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "  Wrapper: $WRAPPER_TMPFILE"
    echo "  Mount-before-install args: ${MOUNT_BEFORE_INSTALL_ARGS[*]}"
    echo "  Mount args: ${MOUNT_ARGS[*]}"
    echo "  WordPress version: ${PLAYGROUND_WORDPRESS_VERSION}"
    echo "  WordPress install mode: ${PLAYGROUND_WORDPRESS_INSTALL_MODE}"
fi

BENCH_TMPFILE=$(mktemp)

set +e
"$PLAYGROUND_CLI" php \
    "${MOUNT_BEFORE_INSTALL_ARGS[@]}" \
    "${MOUNT_ARGS[@]}" \
    "--mount" "${WRAPPER_TMPFILE}:/runner.php" \
    "${BLUEPRINT_ARGS[@]}" \
    "--wordpress-install-mode=${PLAYGROUND_WORDPRESS_INSTALL_MODE}" \
    "--wp=${PLAYGROUND_WORDPRESS_VERSION}" \
    --verbosity=normal \
    -- /runner.php \
    2>&1 | tee "$BENCH_TMPFILE"
playground_exit=${PIPESTATUS[0]}
set -e

rm -f "$WRAPPER_TMPFILE" "$BLUEPRINT_TMPFILE"

BENCH_LOG=""
if [ -f "$RESULT_LOG" ]; then
    BENCH_LOG=$(cat "$RESULT_LOG")
fi
BENCH_STDOUT=$(cat "$BENCH_TMPFILE")
rm -f "$BENCH_TMPFILE"

# ---------------------------------------------------------------------------
# Failure classification (subset of test-runner's case ladder — bench has
# fewer failure modes because there's no PHPUnit, just bootstrap + workloads).
# ---------------------------------------------------------------------------

dump_diagnostics() {
    local label="$1"
    # Write to stderr so the diagnostics survive homeboy core's CI bench
    # progress mode, which calls the runner with `passthrough(false)` and
    # `stderr_passthrough(false)` and only persists stderr in the failure
    # envelope's `stderr_tail`. Echoing to stdout buried every Playground
    # boot/workload failure inside the suppressed stdout buffer, which made
    # `bench.json` show `failure.stderr_tail: ""` for every CI failure mode
    # downstream of the Playground CLI invocation. Stderr is captured AND
    # streamed to the GitHub Actions log, which is exactly what consumers
    # of the bench JSON envelope expect for failure triage.
    {
        echo ""
        echo "============================================"
        echo "$label"
        echo "============================================"
        if [ -n "$BENCH_LOG" ]; then
            echo ""
            echo "--- Structured log ($RESULT_LOG) ---"
            echo "$BENCH_LOG"
        fi
        if [ -n "$BENCH_STDOUT" ]; then
            echo ""
            echo "--- Playground stdout/stderr ---"
            echo "$BENCH_STDOUT"
        fi
    } >&2
}

# Case 1: bootstrap failure captured in the structured log.
if echo "$BENCH_LOG" | grep -qE '^STAGE_(FAIL|FATAL):'; then
    FAILED_STAGE_LINE=$(echo "$BENCH_LOG" | grep -E '^STAGE_(FAIL|FATAL):' | head -1)
    FAILED_STAGE_DETAIL=$(echo "$FAILED_STAGE_LINE" | sed -E 's/^STAGE_(FAIL|FATAL)://')
    FAILED_STEP="Bench bootstrap (${FAILED_STAGE_DETAIL%%:*} stage)"
    FAILURE_OUTPUT="$FAILED_STAGE_LINE"
    dump_diagnostics "BOOTSTRAP FAILURE: $FAILED_STAGE_DETAIL"
    rm -f "$RESULT_LOG"
    exit ${playground_exit:-1}
fi

# Case 2: PHP crashed before the runner could write to the result file.
if [ $playground_exit -ne 0 ] && echo "$BENCH_STDOUT" | grep -qE '^(PHP Parse error|Parse error:|PHP Fatal error|Fatal error:)'; then
    FAILED_STEP="Playground PHP crash (before bench runner took control)"
    FAILURE_OUTPUT=$(echo "$BENCH_STDOUT" | grep -E '^(PHP Parse error|Parse error:|PHP Fatal error|Fatal error:)' | head -5)
    dump_diagnostics "PHP CRASH"
    rm -f "$RESULT_LOG" "$RESULT_JSON"
    exit $playground_exit
fi

# Case 3: playground exited non-zero, no structured failure visible.
if [ $playground_exit -ne 0 ]; then
    FAILED_STEP="Playground exited with code $playground_exit (unclassified)"
    dump_diagnostics "UNCLASSIFIED PLAYGROUND FAILURE (exit=$playground_exit)"
    rm -f "$RESULT_LOG" "$RESULT_JSON"
    exit $playground_exit
fi

# Case 4: the JSON envelope wasn't written. That's a runner bug.
if [ ! -f "$RESULT_JSON" ]; then
    dump_diagnostics "BENCH RUN COMPLETED BUT WROTE NO RESULTS"
    FAILED_STEP="Bench completed but no .pg-bench-results.json emitted"
    rm -f "$RESULT_LOG"
    exit 1
fi

# Surface non-fatal NOTICEs even on success — they're often the early
# warning sign for the next regression.
if echo "$BENCH_LOG" | grep -q "^NOTICE:"; then
    echo ""
    echo "--- Bootstrap notices (non-fatal) ---"
    echo "$BENCH_LOG" | grep "^NOTICE:"
fi

# Hand the JSON envelope to homeboy core. If the env var isn't set
# (direct invocation), leave the file in place under the plugin and
# print the path so a human can inspect it.
if [ -n "${HOMEBOY_BENCH_RESULTS_FILE:-}" ]; then
    cp "$RESULT_JSON" "${HOMEBOY_BENCH_RESULTS_FILE}"
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: [bench:playground] Results copied to ${HOMEBOY_BENCH_RESULTS_FILE}"
    fi
else
    echo ""
    echo "Bench results: $RESULT_JSON"
fi

rm -f "$RESULT_LOG"

echo ""
echo "Playground bench run complete."
