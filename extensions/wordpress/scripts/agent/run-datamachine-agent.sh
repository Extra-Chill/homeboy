#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKLOAD_PATH="$SCRIPT_DIR/datamachine-agent-workload.php"
WORKLOAD_PLAYGROUND_PATH="/homeboy-extension/scripts/agent/datamachine-agent-workload.php"

CONFIG_PATH="${HOMEBOY_DATAMACHINE_AGENT_CONFIG_PATH:-}"
if [ -z "$CONFIG_PATH" ] && [ "${1:-}" != "" ]; then
    CONFIG_PATH="$1"
fi
if [ -z "$CONFIG_PATH" ] || [ ! -s "$CONFIG_PATH" ]; then
    echo "ERROR: pass a Data Machine agent config JSON path as argv[1] or HOMEBOY_DATAMACHINE_AGENT_CONFIG_PATH" >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required" >&2
    exit 1
fi
if [ ! -f "$WORKLOAD_PATH" ]; then
    echo "ERROR: Data Machine agent workload missing at $WORKLOAD_PATH" >&2
    exit 1
fi

RUNTIME_DIR=""
cleanup() {
    if [ -n "$RUNTIME_DIR" ]; then
        rm -rf "$RUNTIME_DIR"
    fi
}
trap cleanup EXIT

if [ -z "${HOMEBOY_RUNTIME_BENCH_HELPER_SH:-}" ] || [ -z "${HOMEBOY_RUNTIME_BENCH_HELPER_PHP:-}" ]; then
    RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-datamachine-agent-runtime.XXXXXX")
    if [ -z "${HOMEBOY_RUNTIME_BENCH_HELPER_SH:-}" ]; then
        cat > "$RUNTIME_DIR/bench-helper.sh" <<'SH'
#!/usr/bin/env bash
homeboy_write_empty_bench_results() {
    printf '{"component":"%s","iterations":%s,"scenarios":[]}\n' "$1" "$2" > "$3"
}
SH
        export HOMEBOY_RUNTIME_BENCH_HELPER_SH="$RUNTIME_DIR/bench-helper.sh"
    fi
    if [ -z "${HOMEBOY_RUNTIME_BENCH_HELPER_PHP:-}" ]; then
        cat > "$RUNTIME_DIR/bench-helper.php" <<'PHP'
<?php
function homeboy_bench_percentile(array $sorted_values, float $p): float {
    $n = count($sorted_values);
    if ($n === 0) {
        return 0.0;
    }
    if ($n === 1) {
        return (float) $sorted_values[0];
    }
    $rank = $p * ($n - 1);
    $lo = (int) floor($rank);
    $hi = (int) ceil($rank);
    if ($lo === $hi) {
        return (float) $sorted_values[$lo];
    }
    $frac = $rank - $lo;
    return (float) ($sorted_values[$lo] * (1 - $frac) + $sorted_values[$hi] * $frac);
}
function homeboy_bench_scenario_id(string $basename): string {
    $name = preg_replace('/\.[^.]+$/', '', $basename);
    $name = preg_replace('/([a-z0-9])([A-Z])/', '$1-$2', $name);
    $name = strtolower($name);
    $name = preg_replace('/[^a-z0-9]+/', '-', $name);
    return trim($name, '-');
}
function homeboy_write_bench_results(string $results_path, string $component_id, int $iterations, array $scenarios): void {
    file_put_contents($results_path, json_encode([
        'component_id' => $component_id,
        'iterations' => $iterations,
        'scenarios' => $scenarios,
    ], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES));
}
PHP
        export HOMEBOY_RUNTIME_BENCH_HELPER_PHP="$RUNTIME_DIR/bench-helper.php"
    fi
fi

RESULTS_FILE="${HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE:-${HOMEBOY_BENCH_RESULTS_FILE:-}}"
if [ -z "$RESULTS_FILE" ]; then
    RESULTS_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-datamachine-agent.XXXXXX")
    PRINT_RESULTS=1
else
    PRINT_RESULTS=0
fi

COMPONENT_PATH=$(jq -r '.component_path // env.HOMEBOY_COMPONENT_PATH // empty' "$CONFIG_PATH")
COMPONENT_ID=$(jq -r '.component_id // env.HOMEBOY_COMPONENT_ID // empty' "$CONFIG_PATH")
if [ -z "$COMPONENT_PATH" ]; then
    COMPONENT_PATH="$(pwd)"
fi
if [ -z "$COMPONENT_ID" ]; then
    COMPONENT_ID="$(basename "$COMPONENT_PATH")"
fi

CONFIG_JSON=$(jq -c . "$CONFIG_PATH")
BUNDLE_REPO=$(jq -r '.bundle_repo // empty' "$CONFIG_PATH")
if [ -n "$BUNDLE_REPO" ]; then
    BUNDLE_REF=$(jq -r '.bundle_ref // "main"' "$CONFIG_PATH")
    BUNDLE_PATH_IN_REPO=$(jq -r '.bundle_path_in_repo // "."' "$CONFIG_PATH")
    if ! command -v git >/dev/null 2>&1; then
        echo "ERROR: git required for bundle_repo" >&2
        exit 1
    fi
    if [ -z "$RUNTIME_DIR" ]; then
        RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-datamachine-agent-runtime.XXXXXX")
    fi
    BUNDLE_CHECKOUT_DIR="$RUNTIME_DIR/bundle-repo"
    git clone --quiet "$BUNDLE_REPO" "$BUNDLE_CHECKOUT_DIR"
    git -C "$BUNDLE_CHECKOUT_DIR" checkout --quiet "$BUNDLE_REF"
    BUNDLE_PATH="$BUNDLE_CHECKOUT_DIR/$BUNDLE_PATH_IN_REPO"
    if [ ! -d "$BUNDLE_PATH" ]; then
        echo "ERROR: bundle_path_in_repo does not exist in bundle_repo: $BUNDLE_PATH_IN_REPO" >&2
        exit 1
    fi
    BUNDLE_GUEST_SLUG="$(basename "$(cd "$BUNDLE_PATH" && pwd)")"
    BUNDLE_GUEST_PATH="/wordpress/wp-content/plugins/${BUNDLE_GUEST_SLUG}"
    CONFIG_JSON=$(jq -c \
        --arg bundlePath "$BUNDLE_PATH" \
        --arg bundleGuestPath "$BUNDLE_GUEST_PATH" \
        --arg bundleRepo "$BUNDLE_REPO" \
        --arg bundleRef "$BUNDLE_REF" \
        --arg bundlePathInRepo "$BUNDLE_PATH_IN_REPO" \
        '. + {
            bundle_path: $bundleGuestPath,
            bundle_host_path: $bundlePath,
            bundle_repo: $bundleRepo,
            bundle_ref: $bundleRef,
            bundle_path_in_repo: $bundlePathInRepo
        } | .validation_dependencies = (
            (if (.validation_dependencies // .dependencies // []) | type == "array" then (.validation_dependencies // .dependencies // []) elif (.validation_dependencies // .dependencies // null) | type == "string" then [(.validation_dependencies // .dependencies)] else [] end)
            + [$bundlePath]
        )' <<<"$CONFIG_JSON")
fi

WORKLOAD_ID=$(jq -r '.workload_id // "datamachine-agent"' "$CONFIG_PATH")
WORKLOAD_LABEL=$(jq -r '.workload_label // "Run Data Machine agent"' "$CONFIG_PATH")
PLAYGROUND_WORDPRESS_VERSION=$(jq -r '.playground_wordpress_version // "7.0"' "$CONFIG_PATH")
BENCH_WARMUP_ITERATIONS=$(jq -r '.bench_warmup_iterations // 0' "$CONFIG_PATH")

SETTINGS_JSON=$(jq -nc \
    --arg workloadPath "$WORKLOAD_PLAYGROUND_PATH" \
    --arg workloadId "$WORKLOAD_ID" \
    --arg workloadLabel "$WORKLOAD_LABEL" \
    --arg wordpressVersion "$PLAYGROUND_WORDPRESS_VERSION" \
    --argjson config "$CONFIG_JSON" \
    --argjson warmup "$BENCH_WARMUP_ITERATIONS" \
    '{
        workload_run_before: ($config.workload_run_before // $config.bootstrap_run // []),
        workload_run_after: ($config.workload_run_after // []),
        validation_dependencies: ($config.validation_dependencies // $config.dependencies // []),
        playground_wordpress_version: $wordpressVersion,
        wp_config_defines: ($config.wp_config_defines // {}),
        playground_file_mounts: ($config.playground_file_mounts // []),
        playground_blueprint: ($config.playground_blueprint // {}),
        bench_site_mode: ($config.bench_site_mode // "fresh"),
        bench_warmup_iterations: $warmup,
        bench_env: (($config.bench_env // {}) + { HOMEBOY_DATAMACHINE_AGENT_CONFIG: ($config | tojson) }),
        playground_workloads: [
            {
                id: $workloadId,
                label: $workloadLabel,
                run: (($config.workload_run_before // $config.bootstrap_run // []) + [
                    { type: "php", file: $workloadPath }
                ] + ($config.workload_run_after // []))
            }
        ]
    }')

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_FILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS="$BENCH_WARMUP_ITERATIONS" \
HOMEBOY_COMPONENT_ID="$COMPONENT_ID" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_PATH" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "$EXTENSION_PATH/scripts/bench/bench-runner.sh"

if [ "$PRINT_RESULTS" = "1" ]; then
    cat "$RESULTS_FILE"
fi

scenario='.scenarios[] | select(.id == "'"$WORKLOAD_ID"'")'
if ! jq -e "$scenario | .metrics.config_present_mean == 1" "$RESULTS_FILE" >/dev/null; then
    echo "ERROR: Data Machine agent workload did not run" >&2
    exit 1
fi

if jq -e "$scenario | .metadata.error?" "$RESULTS_FILE" >/dev/null; then
    echo "ERROR: Data Machine agent workload reported an error" >&2
    jq -r "$scenario | .metadata.error" "$RESULTS_FILE" >&2
    exit 1
fi

# After this script returns, consumers can extract engine_data fields with:
#
#   "$EXTENSION_PATH/scripts/agent/extract-engine-data.sh" \
#       --results "$RESULTS_FILE" \
#       --scenario <scenario-id> \
#       --field key=metadata.engine_data.path.to.value
