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

FAILED_STEP=""
FAILURE_OUTPUT=""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
PHP_PREFLIGHT_HELPER="${SCRIPT_DIR}/../lib/php-preflight.sh"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
# shellcheck source=../lib/php-preflight.sh
if [ -f "$PHP_PREFLIGHT_HELPER" ]; then
    source "$PHP_PREFLIGHT_HELPER"
fi
# shellcheck source=../lib/validation-dependencies.sh
if [ -f "$DEPENDENCY_HELPER" ]; then
    source "$DEPENDENCY_HELPER"
fi

print_failure_summary() {
    if [ -n "$FAILED_STEP" ]; then
        echo ""
        echo "============================================"
        echo "BENCH FAILED: $FAILED_STEP"
        echo "============================================"
        if [ -n "$FAILURE_OUTPUT" ]; then
            echo ""
            echo "Error details:"
            echo "$FAILURE_OUTPUT"
        fi
    fi
}
trap print_failure_summary EXIT

# Component resolution — same priority order as test-runner-playground.sh
# so bench gets the same component-discovery semantics for free.
if [ -n "${HOMEBOY_COMPONENT_PATH:-}" ]; then
    PLUGIN_PATH="${HOMEBOY_COMPONENT_PATH}"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-}"
elif [ -n "${HOMEBOY_PROJECT_PATH:-}" ]; then
    PLUGIN_PATH="${HOMEBOY_PROJECT_PATH}"
    COMPONENT_ID=""
else
    PLUGIN_PATH="$(pwd)"
    COMPONENT_ID="$(basename "$PLUGIN_PATH")"
fi

PLUGIN_SLUG="$(basename "$PLUGIN_PATH")"
COMPONENT_ID="${COMPONENT_ID:-$PLUGIN_SLUG}"

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

BENCH_DIR="${PLUGIN_PATH}/tests/bench"
if [ ! -d "$BENCH_DIR" ]; then
    echo ""
    echo "⚠ No tests/bench directory found at ${BENCH_DIR}"
    echo "  Skipping bench run — nothing to measure."
    echo ""
    # Emit an empty-but-valid BenchResults so homeboy core's parser
    # doesn't see a missing file and treat the run as a crash.
    if [ -n "${HOMEBOY_BENCH_RESULTS_FILE:-}" ]; then
        cat > "${HOMEBOY_BENCH_RESULTS_FILE}" <<EMPTY
{"component_id":"${COMPONENT_ID}","iterations":0,"scenarios":[]}
EMPTY
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

ITERATIONS="${HOMEBOY_BENCH_ITERATIONS:-10}"

# ---------------------------------------------------------------------------
# Build VFS mount args (mirrors test-runner-playground.sh shape)
# ---------------------------------------------------------------------------
MOUNT_ARGS=()
MOUNT_ARGS+=("--mount" "${PLUGIN_PATH}:/wordpress/wp-content/plugins/${PLUGIN_SLUG}")

if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(basename "$dep_path")"
        MOUNT_ARGS+=("--mount" "${dep_path}:/wordpress/wp-content/plugins/${dep_slug}")
    done <<< "$DEPENDENCY_PATHS"
fi

PLUGIN_DB_PHP="${PLUGIN_PATH}/db.php"
if [ -f "$PLUGIN_DB_PHP" ]; then
    MOUNT_ARGS+=("--mount" "${PLUGIN_DB_PHP}:/wordpress/wp-content/db.php")
fi

MOUNT_ARGS+=("--mount" "${EXTENSION_PATH}:/homeboy-extension")

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

INSTANCE_ID="${HOMEBOY_BENCH_INSTANCE_ID:-0}"
CONCURRENCY="${HOMEBOY_BENCH_CONCURRENCY:-1}"

PLAYGROUND_DEP_MOUNTS=""
if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(basename "$dep_path")"
        if [ -n "$PLAYGROUND_DEP_MOUNTS" ]; then
            PLAYGROUND_DEP_MOUNTS+="\\n"
        fi
        PLAYGROUND_DEP_MOUNTS+="/wordpress/wp-content/plugins/${dep_slug}"
    done <<< "$DEPENDENCY_PATHS"
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
sed \
    -e "s|{{PLUGIN_SLUG}}|${PLUGIN_SLUG}|g" \
    -e "s|{{COMPONENT_ID}}|${COMPONENT_ID}|g" \
    -e "s|{{ITERATIONS}}|${ITERATIONS}|g" \
    -e "s|{{PLAYGROUND_DEP_MOUNTS}}|${PLAYGROUND_DEP_MOUNTS}|g" \
    -e "s|{{SHARED_STATE_PATH}}|${SHARED_STATE_GUEST}|g" \
    -e "s|{{INSTANCE_ID}}|${INSTANCE_ID}|g" \
    -e "s|{{CONCURRENCY}}|${CONCURRENCY}|g" \
    -e "s|{{RESULT_SUFFIX}}|${RESULT_SUFFIX}|g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{WP_CONFIG_DEFINES_JSON}}${WP_CONFIG_DEFINES_DELIM}${WP_CONFIG_DEFINES_JSON}${WP_CONFIG_DEFINES_DELIM}g" \
    "$TEMPLATE" > "$WRAPPER_TMPFILE"

echo "Running performance benchmarks via WordPress Playground..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Iterations: ${ITERATIONS}"
echo "  Backend: playground (PHP-WASM + SQLite)"
if [ -n "$SHARED_STATE_GUEST" ]; then
    echo "  Shared state: ${SHARED_STATE_HOST} (instance ${INSTANCE_ID}/${CONCURRENCY})"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "  Wrapper: $WRAPPER_TMPFILE"
    echo "  Mount args: ${MOUNT_ARGS[*]}"
fi

BENCH_TMPFILE=$(mktemp)

set +e
"$PLAYGROUND_CLI" php \
    "${MOUNT_ARGS[@]}" \
    "--mount" "${WRAPPER_TMPFILE}:/runner.php" \
    --wp=6.9 \
    --verbosity=normal \
    -- /runner.php \
    2>&1 | tee "$BENCH_TMPFILE"
playground_exit=${PIPESTATUS[0]}
set -e

rm -f "$WRAPPER_TMPFILE"

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
