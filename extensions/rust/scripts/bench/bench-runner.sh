#!/usr/bin/env bash
set -euo pipefail

# Rust bench runner for `homeboy bench`.
#
# Discovers bench binaries declared in the component's Cargo.toml
# (`[[bin]]` entries with `name = "bench-*"` or `src/bin/bench-*.rs`
# files) and executes each via `cargo run --release --bin bench-<id>`.
# Aggregates per-binary timing JSON into the BenchResults envelope
# homeboy core expects.
#
# CONTRACT (component side)
#
# Each bench binary must:
#   1. Read iteration count from env var HOMEBOY_BENCH_ITERATIONS
#      (default 10 if unset).
#   2. Run its measured workload that many times, timing each iteration
#      with std::time::Instant.
#   3. Emit a single JSON object to stdout (last line of output) with shape:
#
#        {
#          "timings_ns": [12345, 12678, 12112, ...],
#          "peak_rss_bytes": 41943040
#        }
#
#      timings_ns: array of per-iteration nanosecond durations
#                  (length must equal HOMEBOY_BENCH_ITERATIONS)
#      peak_rss_bytes: optional, max RSS across iterations
#
#   4. Exit 0 on success, non-zero on failure. Stderr is captured for
#      diagnostics but ignored for results.
#
# A reference workload helper crate (homeboy-bench-rs) is planned to
# eliminate the boilerplate, but the contract above is the source of truth —
# components without a helper dep can implement it directly in ~20 lines.
#
# CONTRACT (homeboy core side)
#
# Standard env vars set by core:
#   HOMEBOY_EXTENSION_PATH       — path to this extension
#   HOMEBOY_COMPONENT_PATH       — path to the Rust project root (Cargo.toml)
#   HOMEBOY_COMPONENT_ID         — component identifier
#   HOMEBOY_BENCH_ITERATIONS     — iterations per workload (default 10)
#   HOMEBOY_BENCH_RESULTS_FILE   — where core wants the envelope written
#   HOMEBOY_BENCH_LIST_ONLY      — when 1, emit scenario inventory only
#   HOMEBOY_DEBUG                — verbose output

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASH_PREFLIGHT_HELPER="${HOMEBOY_RUNTIME_BASH_PREFLIGHT:-${SCRIPT_DIR}/../lib/bash-preflight.sh}"
# shellcheck source=/dev/null
source "$BASH_PREFLIGHT_HELPER"
homeboy_require_bash_version 4

RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context
# shellcheck source=/dev/null
if [ -n "$FAILURE_TRAP_HELPER" ] && [ -f "$FAILURE_TRAP_HELPER" ]; then
    source "$FAILURE_TRAP_HELPER"
    homeboy_init_failure_trap
else
    FAILED_STEP=""
    FAILURE_OUTPUT=""
fi

ITERATIONS="${HOMEBOY_BENCH_ITERATIONS:-10}"
RESULTS_FILE="${HOMEBOY_BENCH_RESULTS_FILE:-${PROJECT_PATH}/.rust-bench-results.json}"
LIST_ONLY="${HOMEBOY_BENCH_LIST_ONLY:-0}"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [bench:rust] extension=$EXTENSION_PATH" >&2
    echo "DEBUG: [bench:rust] project=$PROJECT_PATH" >&2
    echo "DEBUG: [bench:rust] component_id=$COMPONENT_ID" >&2
    echo "DEBUG: [bench:rust] iterations=$ITERATIONS" >&2
    echo "DEBUG: [bench:rust] results=$RESULTS_FILE" >&2
    echo "DEBUG: [bench:rust] list_only=$LIST_ONLY" >&2
fi

if [ ! -f "${PROJECT_PATH}/Cargo.toml" ]; then
    FAILED_STEP="not a Rust project"
    FAILURE_OUTPUT="No Cargo.toml at ${PROJECT_PATH}"
    exit 1
fi

# Verify cargo is available.
if ! command -v cargo >/dev/null 2>&1; then
    FAILED_STEP="cargo not on PATH"
    FAILURE_OUTPUT="Install Rust toolchain: https://rustup.rs"
    exit 1
fi

# ── Discover bench binaries ────────────────────────────────────────
#
# A bench workload is a Cargo binary whose name starts with `bench-`.
# Cargo recognizes binaries in two places:
#   1. [[bin]] entries in Cargo.toml
#   2. src/bin/<name>.rs (auto-discovered)
# Both manifest paths are supported.

discover_bench_bins() {
    local _bins=()

    # 1. Auto-discovered: src/bin/bench-*.rs
    if [ -d "${PROJECT_PATH}/src/bin" ]; then
        while IFS= read -r f; do
            local _name
            _name="$(basename "$f" .rs)"
            if [[ "$_name" == bench-* ]]; then
                _bins+=("$_name")
            fi
        done < <(find "${PROJECT_PATH}/src/bin" -maxdepth 1 -name 'bench-*.rs' -type f 2>/dev/null | sort)
    fi

    # 2. Explicit [[bin]] entries with name = "bench-*"
    # Use cargo metadata for robust parsing (handles workspaces, etc.).
    if command -v jq >/dev/null 2>&1; then
        while IFS= read -r _name; do
            [ -n "$_name" ] || continue
            # Avoid duplicates from method 1.
            local _seen=0
            for _existing in "${_bins[@]:-}"; do
                if [ "$_existing" = "$_name" ]; then _seen=1; break; fi
            done
            [ "$_seen" = "0" ] && _bins+=("$_name")
        done < <(cargo metadata --no-deps --format-version=1 --manifest-path="${PROJECT_PATH}/Cargo.toml" 2>/dev/null \
            | jq -r '.packages[].targets[] | select(.kind[0] == "bin") | select(.name | startswith("bench-")) | .name')
    fi

    printf '%s\n' "${_bins[@]:-}"
}

bench_bin_file() {
    local _bin="$1"
    local _auto="${PROJECT_PATH}/src/bin/${_bin}.rs"
    if [ -f "$_auto" ]; then
        printf 'src/bin/%s.rs\n' "$_bin"
        return 0
    fi
    printf 'null\n'
}

mapfile -t BENCH_BINS < <(discover_bench_bins)

if [ "$LIST_ONLY" = "1" ]; then
    if ! command -v python3 >/dev/null 2>&1; then
        FAILED_STEP="python3 not on PATH"
        FAILURE_OUTPUT="bench list requires python3 to write the discovery envelope"
        exit 1
    fi

    SCENARIO_ARGS=()
    for _bin in "${BENCH_BINS[@]:-}"; do
        _scenario_id="${_bin#bench-}"
        _scenario_file="$(bench_bin_file "$_bin")"
        SCENARIO_ARGS+=("${_scenario_id}=${_scenario_file}")
    done

    python3 - <<PYTHON_LIST "$RESULTS_FILE" "$COMPONENT_ID" "$ITERATIONS" "${SCENARIO_ARGS[@]:-}"
import json, os, sys

results_file = sys.argv[1]
component_id = sys.argv[2]
iterations = int(sys.argv[3])
scenario_kvs = sys.argv[4:]
scenarios = []
for kv in scenario_kvs:
    scenario_id, rel_file = kv.split('=', 1)
    scenario = {
        'id': scenario_id,
        'iterations': 0,
        'default_iterations': iterations,
        'tags': [],
        'metrics': {},
    }
    if rel_file != 'null':
        scenario['file'] = rel_file
        scenario['source'] = 'in_tree'
    scenarios.append(scenario)

os.makedirs(os.path.dirname(results_file) or '.', exist_ok=True)
with open(results_file, 'w', encoding='utf-8') as fh:
    json.dump({'component_id': component_id, 'iterations': 0, 'scenarios': scenarios}, fh, indent=2)
PYTHON_LIST

    echo "Discovered ${#BENCH_BINS[@]} Rust bench scenarios."
    exit 0
fi

if [ "${#BENCH_BINS[@]}" -eq 0 ]; then
    echo "" >&2
    echo "⚠ No bench binaries found at ${PROJECT_PATH}" >&2
    echo "  Bench binaries must be named 'bench-*' and live at" >&2
    echo "  src/bin/bench-*.rs or be declared as [[bin]] entries in Cargo.toml." >&2
    echo "" >&2
    cat > "$RESULTS_FILE" <<EMPTY
{"component_id":"${COMPONENT_ID}","iterations":0,"scenarios":[]}
EMPTY
    exit 0
fi

echo "Running Rust benchmarks..."
echo "  Component: ${COMPONENT_ID} (${PROJECT_PATH})"
echo "  Iterations: ${ITERATIONS}"
echo "  Discovered: ${#BENCH_BINS[@]} bench binaries"

# ── Run each bench binary ──────────────────────────────────────────

# Build all bench binaries up front so the per-binary loop only pays
# cargo's metadata-resolution cost, not a fresh compile each time.
echo ""
echo "Building bench binaries (release)..."
BUILD_ARGS=(build --release --manifest-path="${PROJECT_PATH}/Cargo.toml")
for _bin in "${BENCH_BINS[@]}"; do
    BUILD_ARGS+=(--bin "$_bin")
done
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    cargo "${BUILD_ARGS[@]}"
else
    cargo "${BUILD_ARGS[@]}" 2>&1 | tail -5 || {
        FAILED_STEP="cargo build"
        exit 1
    }
fi

SCENARIOS_JSON_TMPDIR="$(mktemp -d)"
cleanup_scenarios() {
    rm -rf "$SCENARIOS_JSON_TMPDIR"
    if type homeboy_print_failure_summary >/dev/null 2>&1; then
        homeboy_print_failure_summary
    fi
}
trap cleanup_scenarios EXIT

OVERALL_OK=1
SCENARIOS_PATHS=()
for _bin in "${BENCH_BINS[@]}"; do
    # scenario id = bin name minus "bench-" prefix
    _scenario_id="${_bin#bench-}"
    _scenario_file="${SCENARIOS_JSON_TMPDIR}/${_scenario_id}.json"

    echo ""
    echo "WORKLOAD_BEGIN: ${_scenario_id} (${_bin})"

    set +e
    HOMEBOY_BENCH_ITERATIONS="$ITERATIONS" \
        cargo run --release --quiet --manifest-path="${PROJECT_PATH}/Cargo.toml" \
        --bin "$_bin" > "${_scenario_file}.raw" 2> "${_scenario_file}.err"
    _exit=$?
    set -e

    if [ "$_exit" -ne 0 ]; then
        echo "WORKLOAD_ERROR: ${_scenario_id} — cargo exit $_exit" >&2
        if [ -s "${_scenario_file}.err" ]; then
            echo "  stderr:" >&2
            sed 's/^/    /' "${_scenario_file}.err" >&2
        fi
        OVERALL_OK=0
        continue
    fi

    # Last non-empty stdout line is the JSON payload (lets bench
    # binaries emit progress lines without breaking the contract).
    _payload="$(grep -v '^[[:space:]]*$' "${_scenario_file}.raw" | tail -n 1)"

    if [ -z "$_payload" ]; then
        echo "WORKLOAD_ERROR: ${_scenario_id} — no JSON output on stdout" >&2
        OVERALL_OK=0
        continue
    fi

    echo "$_payload" > "$_scenario_file"
    SCENARIOS_PATHS+=("${_scenario_id}=${_scenario_file}")

    # Quick visual confirmation (best-effort, requires jq).
    if command -v jq >/dev/null 2>&1; then
        _count="$(jq -r '.timings_ns | length' "$_scenario_file" 2>/dev/null || echo "?")"
        echo "WORKLOAD_DONE:  ${_scenario_id} (${_count} iterations measured)"
    else
        echo "WORKLOAD_DONE:  ${_scenario_id}"
    fi
done

# ── Build the BenchResults envelope ────────────────────────────────
#
# Use python3 for the JSON math: jq doesn't have a clean way to do
# percentile interpolation, and we want bit-for-bit parity with the
# WP and Node runners' R-7 method.

if ! command -v python3 >/dev/null 2>&1; then
    FAILED_STEP="python3 not on PATH"
    FAILURE_OUTPUT="bench-runner.sh requires python3 for percentile math"
    exit 1
fi

python3 - <<PYTHON_AGGREGATE "$RESULTS_FILE" "$COMPONENT_ID" "$ITERATIONS" "${SCENARIOS_PATHS[@]:-}"
import json, os, sys

results_file = sys.argv[1]
component_id = sys.argv[2]
iterations   = int(sys.argv[3])
scenario_kvs = sys.argv[4:]

def percentile_r7(sorted_ms, p):
    n = len(sorted_ms)
    if n == 0: return 0.0
    if n == 1: return sorted_ms[0]
    rank = p * (n - 1)
    lo, hi = int(rank), -(-int(rank * 10 ** 9) // 10 ** 9)  # ceil
    hi = min(int(rank) + (1 if rank > int(rank) else 0), n - 1)
    if lo == hi: return sorted_ms[lo]
    frac = rank - lo
    return sorted_ms[lo] * (1 - frac) + sorted_ms[hi] * frac

scenarios = []
for kv in scenario_kvs:
    if not kv: continue
    sid, path = kv.split("=", 1)
    with open(path) as f:
        payload = json.load(f)

    timings_ns = payload.get("timings_ns", [])
    timings_ms = sorted(t / 1_000_000.0 for t in timings_ns)
    n = len(timings_ms)

    if n == 0:
        sys.stderr.write(f"WORKLOAD_WARN: {sid} emitted no timings\n")
        continue

    metrics = {
        "mean_ms": sum(timings_ms) / n,
        "p50_ms":  percentile_r7(timings_ms, 0.50),
        "p95_ms":  percentile_r7(timings_ms, 0.95),
        "p99_ms":  percentile_r7(timings_ms, 0.99),
        "min_ms":  timings_ms[0],
        "max_ms":  timings_ms[-1],
    }

    scenario = {
        "id": sid,
        "iterations": n,
        "metrics": metrics,
    }
    if "peak_rss_bytes" in payload:
        scenario["memory"] = {"peak_bytes": int(payload["peak_rss_bytes"])}

    scenarios.append(scenario)

envelope = {
    "component_id": component_id,
    "iterations":   iterations,
    "scenarios":    scenarios,
}

os.makedirs(os.path.dirname(results_file) or ".", exist_ok=True)
with open(results_file, "w") as f:
    json.dump(envelope, f, indent=2)

print(f"\nResults: {len(scenarios)} scenario(s) written to {results_file}")
PYTHON_AGGREGATE

if [ "$OVERALL_OK" -ne 1 ]; then
    FAILED_STEP="one or more bench binaries failed"
    exit 1
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [bench:rust] envelope written to $RESULTS_FILE" >&2
fi
