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
#   HOMEBOY_BENCH_SCENARIOS      — comma-separated exact scenario ids selected by core
#   HOMEBOY_RUST_BENCH_CRITERION — when 1/true, run Criterion benches and normalize reports
#   HOMEBOY_RUST_BENCH_PROFILES  — when 1/true/all, add Rust clean/warm/changed profiles
#   HOMEBOY_RUST_BENCH_CHANGED_FILE — changed-file profile target (default src/lib.rs or src/main.rs)
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
SELECTED_SCENARIOS="${HOMEBOY_BENCH_SCENARIOS:-}"
CRITERION_REQUEST="${HOMEBOY_RUST_BENCH_CRITERION:-0}"
PROFILE_REQUEST="${HOMEBOY_RUST_BENCH_PROFILES:-0}"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [bench:rust] extension=$EXTENSION_PATH" >&2
    echo "DEBUG: [bench:rust] project=$PROJECT_PATH" >&2
    echo "DEBUG: [bench:rust] component_id=$COMPONENT_ID" >&2
    echo "DEBUG: [bench:rust] iterations=$ITERATIONS" >&2
    echo "DEBUG: [bench:rust] results=$RESULTS_FILE" >&2
    echo "DEBUG: [bench:rust] list_only=$LIST_ONLY" >&2
    echo "DEBUG: [bench:rust] scenarios=${SELECTED_SCENARIOS:-<all>}" >&2
    echo "DEBUG: [bench:rust] criterion=$CRITERION_REQUEST" >&2
    echo "DEBUG: [bench:rust] profiles=$PROFILE_REQUEST" >&2
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

    if [ "${#_bins[@]}" -gt 0 ]; then
        printf '%s\n' "${_bins[@]}"
    fi
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

scenario_selected() {
    local _scenario="$1"
    [ -z "$SELECTED_SCENARIOS" ] && return 0
    case ",${SELECTED_SCENARIOS}," in
        *",${_scenario},"*) return 0 ;;
        *) return 1 ;;
    esac
}

truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|all|ALL) return 0 ;;
        *) return 1 ;;
    esac
}

criterion_requested() {
    truthy "$CRITERION_REQUEST" && return 0
    case ",${SELECTED_SCENARIOS}," in
        *,criterion-*) return 0 ;;
    esac
    return 1
}

profiles_requested() {
    truthy "$PROFILE_REQUEST" && return 0
    case ",${SELECTED_SCENARIOS}," in
        *,rust-clean-build,*|*,rust-warm-build,*|*,rust-changed-file-check,*|*,rust-test,*) return 0 ;;
    esac
    return 1
}

discover_criterion_benches() {
    command -v jq >/dev/null 2>&1 || return 0
    cargo metadata --no-deps --format-version=1 --manifest-path="${PROJECT_PATH}/Cargo.toml" 2>/dev/null \
        | jq -r '.packages[].targets[] | select(.kind[]? == "bench") | .name' \
        | sort -u
}

profile_changed_file() {
    if [ -n "${HOMEBOY_RUST_BENCH_CHANGED_FILE:-}" ]; then
        printf '%s\n' "$HOMEBOY_RUST_BENCH_CHANGED_FILE"
        return 0
    fi
    if [ -f "${PROJECT_PATH}/src/lib.rs" ]; then
        printf 'src/lib.rs\n'
        return 0
    fi
    if [ -f "${PROJECT_PATH}/src/main.rs" ]; then
        printf 'src/main.rs\n'
        return 0
    fi
    return 1
}

PROFILE_IDS=(rust-clean-build rust-warm-build rust-changed-file-check rust-test)

mapfile -t BENCH_BINS < <(discover_bench_bins)
mapfile -t CRITERION_BENCHES < <(discover_criterion_benches)

if [ "$LIST_ONLY" = "1" ]; then
    if ! command -v python3 >/dev/null 2>&1; then
        FAILED_STEP="python3 not on PATH"
        FAILURE_OUTPUT="bench list requires python3 to write the discovery envelope"
        exit 1
    fi

    SCENARIO_ARGS=()
    for _bin in "${BENCH_BINS[@]}"; do
        _scenario_id="${_bin#bench-}"
        scenario_selected "$_scenario_id" || continue
        _scenario_file="$(bench_bin_file "$_bin")"
        SCENARIO_ARGS+=("${_scenario_id}=${_scenario_file}=bench-binary")
    done

    if profiles_requested; then
        for _profile_id in "${PROFILE_IDS[@]}"; do
            scenario_selected "$_profile_id" || continue
            SCENARIO_ARGS+=("${_profile_id}=null=rust-profile")
        done
    fi

    if criterion_requested; then
        for _criterion_bench in "${CRITERION_BENCHES[@]}"; do
            _scenario_id="criterion-${_criterion_bench}"
            scenario_selected "$_scenario_id" || continue
            SCENARIO_ARGS+=("${_scenario_id}=benches/${_criterion_bench}.rs=criterion")
        done
    fi

    python3 - <<PYTHON_LIST "$RESULTS_FILE" "$COMPONENT_ID" "$ITERATIONS" "${SCENARIO_ARGS[@]:-}"
import json, os, sys

results_file = sys.argv[1]
component_id = sys.argv[2]
iterations = int(sys.argv[3])
scenario_kvs = sys.argv[4:]
scenarios = []
for kv in scenario_kvs:
    scenario_id, rel_file, source = kv.split('=', 2)
    scenario = {
        'id': scenario_id,
        'iterations': 0,
        'default_iterations': iterations,
        'tags': [],
        'metrics': {},
        'source': source,
    }
    if rel_file != 'null':
        scenario['file'] = rel_file
    scenarios.append(scenario)

os.makedirs(os.path.dirname(results_file) or '.', exist_ok=True)
with open(results_file, 'w', encoding='utf-8') as fh:
    json.dump({'component_id': component_id, 'iterations': 0, 'scenarios': scenarios}, fh, indent=2)
PYTHON_LIST

    echo "Discovered ${#SCENARIO_ARGS[@]} Rust bench scenarios."
    exit 0
fi

if [ "${#BENCH_BINS[@]}" -eq 0 ] && ! profiles_requested && ! criterion_requested; then
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
if profiles_requested; then
    echo "  Rust profiles: enabled"
fi
if criterion_requested; then
    echo "  Criterion: enabled (${#CRITERION_BENCHES[@]} bench target(s))"
fi

# ── Run each bench binary ──────────────────────────────────────────

# Build all bench binaries up front so the per-binary loop only pays
# cargo's metadata-resolution cost, not a fresh compile each time.
SELECTED_BENCH_BINS=()
for _bin in "${BENCH_BINS[@]}"; do
    _scenario_id="${_bin#bench-}"
    scenario_selected "$_scenario_id" && SELECTED_BENCH_BINS+=("$_bin")
done

if [ "${#SELECTED_BENCH_BINS[@]}" -gt 0 ]; then
    echo ""
    echo "Building bench binaries (release)..."
    BUILD_ARGS=(build --release --manifest-path="${PROJECT_PATH}/Cargo.toml")
    for _bin in "${SELECTED_BENCH_BINS[@]}"; do
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
for _bin in "${SELECTED_BENCH_BINS[@]}"; do
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

run_profile_command() {
    local _scenario_id="$1"
    local _payload_file="$2"
    local _metadata_json="$3"
    shift 3

    python3 - <<'PYTHON_PROFILE' "$_payload_file" "$ITERATIONS" "$_metadata_json" "$@"
import json, os, subprocess, sys, time

payload_file = sys.argv[1]
iterations = int(sys.argv[2])
metadata = json.loads(sys.argv[3])
command = sys.argv[4:]

timings = []
last_stderr = ''
for _ in range(iterations):
    start = time.perf_counter_ns()
    proc = subprocess.run(command, cwd=os.environ['PROJECT_PATH'], text=True, capture_output=True)
    elapsed = time.perf_counter_ns() - start
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr or proc.stdout or f'command exited {proc.returncode}\n')
        sys.exit(proc.returncode)
    last_stderr = proc.stderr
    timings.append(elapsed)

with open(payload_file, 'w', encoding='utf-8') as fh:
    json.dump({
        'timings_ns': timings,
        'metadata': metadata,
        'source': 'rust-profile',
    }, fh, indent=2)
PYTHON_PROFILE
}

run_profile_scenario() {
    local _scenario_id="$1"
    local _scenario_file="${SCENARIOS_JSON_TMPDIR}/${_scenario_id}.json"
    local _changed_rel=""
    local _changed_abs=""
    local _backup_file=""

    scenario_selected "$_scenario_id" || return 0

    echo ""
    echo "WORKLOAD_BEGIN: ${_scenario_id} (Rust profile)"

    case "$_scenario_id" in
        rust-clean-build)
            cargo clean --manifest-path="${PROJECT_PATH}/Cargo.toml" >/dev/null 2>&1 || true
            set +e
            PROJECT_PATH="$PROJECT_PATH" run_profile_command "$_scenario_id" "$_scenario_file" \
                '{"cache_mode":"clean","change_mode":"none","command":"cargo build --release"}' \
                cargo build --release --manifest-path="${PROJECT_PATH}/Cargo.toml" >"${_scenario_file}.out" 2>"${_scenario_file}.err"
            _exit=$?
            set -e
            ;;
        rust-warm-build)
            cargo build --release --manifest-path="${PROJECT_PATH}/Cargo.toml" >/dev/null 2>&1 || true
            set +e
            PROJECT_PATH="$PROJECT_PATH" run_profile_command "$_scenario_id" "$_scenario_file" \
                '{"cache_mode":"warm","change_mode":"none","command":"cargo build --release"}' \
                cargo build --release --manifest-path="${PROJECT_PATH}/Cargo.toml" >"${_scenario_file}.out" 2>"${_scenario_file}.err"
            _exit=$?
            set -e
            ;;
        rust-changed-file-check)
            if ! _changed_rel="$(profile_changed_file)"; then
                echo "WORKLOAD_SKIP: ${_scenario_id} — no src/lib.rs or src/main.rs changed-file target" >&2
                return 0
            fi
            _changed_abs="${PROJECT_PATH}/${_changed_rel}"
            if [ ! -f "$_changed_abs" ]; then
                echo "WORKLOAD_ERROR: ${_scenario_id} — changed file not found: ${_changed_rel}" >&2
                OVERALL_OK=0
                return 0
            fi
            _backup_file="${SCENARIOS_JSON_TMPDIR}/${_scenario_id}.backup"
            cp "$_changed_abs" "$_backup_file"
            printf '\n// homeboy bench changed-file profile touch\n' >> "$_changed_abs"
            set +e
            PROJECT_PATH="$PROJECT_PATH" run_profile_command "$_scenario_id" "$_scenario_file" \
                "{\"cache_mode\":\"warm\",\"change_mode\":\"changed_file\",\"changed_file\":\"${_changed_rel}\",\"command\":\"cargo check\"}" \
                cargo check --manifest-path="${PROJECT_PATH}/Cargo.toml" >"${_scenario_file}.out" 2>"${_scenario_file}.err"
            _exit=$?
            set -e
            cp "$_backup_file" "$_changed_abs"
            ;;
        rust-test)
            cargo test --no-run --manifest-path="${PROJECT_PATH}/Cargo.toml" >/dev/null 2>&1 || true
            set +e
            PROJECT_PATH="$PROJECT_PATH" run_profile_command "$_scenario_id" "$_scenario_file" \
                '{"cache_mode":"warm","change_mode":"none","command":"cargo test --no-run"}' \
                cargo test --no-run --manifest-path="${PROJECT_PATH}/Cargo.toml" >"${_scenario_file}.out" 2>"${_scenario_file}.err"
            _exit=$?
            set -e
            ;;
        *)
            return 0
            ;;
    esac

    if [ "${_exit:-1}" -ne 0 ]; then
        echo "WORKLOAD_ERROR: ${_scenario_id} — profile command exit ${_exit}" >&2
        if [ -s "${_scenario_file}.err" ]; then
            echo "  stderr:" >&2
            sed 's/^/    /' "${_scenario_file}.err" >&2
        fi
        OVERALL_OK=0
        return 0
    fi

    SCENARIOS_PATHS+=("${_scenario_id}=${_scenario_file}")
    echo "WORKLOAD_DONE:  ${_scenario_id} (${ITERATIONS} iterations measured)"
}

if profiles_requested; then
    for _profile_id in "${PROFILE_IDS[@]}"; do
        run_profile_scenario "$_profile_id"
    done
fi

normalize_criterion_reports() {
    local _bench_name="$1"
    local _artifact_root="${PROJECT_PATH}/target/criterion"
    [ -d "$_artifact_root" ] || return 0

    python3 - <<'PYTHON_CRITERION' "$SCENARIOS_JSON_TMPDIR" "$PROJECT_PATH" "$_bench_name" "$ITERATIONS"
import json, os, pathlib, re, sys

out_dir = pathlib.Path(sys.argv[1])
project = pathlib.Path(sys.argv[2])
bench_name = sys.argv[3]
iterations = int(sys.argv[4])
criterion_root = project / 'target' / 'criterion'

def slug(value):
    return re.sub(r'[^A-Za-z0-9_.-]+', '-', value).strip('-').lower() or 'criterion'

estimate_files = sorted(criterion_root.glob('**/new/estimates.json'))
for estimates in estimate_files:
    rel = estimates.relative_to(project)
    parts = list(estimates.relative_to(criterion_root).parts[:-2])
    if not parts:
        continue
    sid = 'criterion-' + slug(bench_name if len(estimate_files) == 1 else f"{bench_name}-{'-'.join(parts)}")
    scenario_file = out_dir / f'{sid}.json'
    with estimates.open(encoding='utf-8') as fh:
        data = json.load(fh)
    mean = data.get('mean', {}).get('point_estimate')
    median = data.get('median', {}).get('point_estimate')
    if mean is None and median is None:
        continue
    lower = data.get('mean', {}).get('confidence_interval', {}).get('lower_bound', mean or median)
    upper = data.get('mean', {}).get('confidence_interval', {}).get('upper_bound', mean or median)
    point = mean if mean is not None else median
    timings = [int(point)] * max(1, iterations)
    payload = {
        'timings_ns': timings,
        'metadata': {
            'adapter': 'criterion',
            'criterion_bench': bench_name,
            'criterion_id': '/'.join(parts),
        },
        'artifacts': {
            'criterion_estimates': {
                'path': str(rel),
                'kind': 'json',
                'label': 'Criterion estimates',
            },
        },
        'metrics': {
            'criterion_mean_ms': (mean or point) / 1_000_000.0,
            'criterion_median_ms': (median or point) / 1_000_000.0,
            'criterion_mean_ci_lower_ms': (lower or point) / 1_000_000.0,
            'criterion_mean_ci_upper_ms': (upper or point) / 1_000_000.0,
        },
        'source': 'criterion',
        'file': str(rel),
    }
    with scenario_file.open('w', encoding='utf-8') as fh:
        json.dump(payload, fh, indent=2)
    print(f'{sid}={scenario_file}')
PYTHON_CRITERION
}

if criterion_requested; then
    if [ "${#CRITERION_BENCHES[@]}" -eq 0 ]; then
        echo "WORKLOAD_WARN: Criterion requested but no Cargo bench targets were discovered." >&2
        echo "  Add [[bench]] entries with harness = false and Criterion dev-dependency, or disable HOMEBOY_RUST_BENCH_CRITERION." >&2
    fi

    for _criterion_bench in "${CRITERION_BENCHES[@]}"; do
        _scenario_id="criterion-${_criterion_bench}"
        if [ -n "$SELECTED_SCENARIOS" ]; then
            case ",${SELECTED_SCENARIOS}," in
                *,criterion-*) ;;
                *) continue ;;
            esac
        fi

        echo ""
        echo "WORKLOAD_BEGIN: ${_scenario_id} (Criterion)"
        set +e
        cargo bench --manifest-path="${PROJECT_PATH}/Cargo.toml" --bench "$_criterion_bench" -- --quiet >"${SCENARIOS_JSON_TMPDIR}/${_scenario_id}.out" 2>"${SCENARIOS_JSON_TMPDIR}/${_scenario_id}.err"
        _exit=$?
        set -e

        if [ "$_exit" -ne 0 ]; then
            echo "WORKLOAD_ERROR: ${_scenario_id} — cargo bench exit $_exit" >&2
            echo "  Criterion support is optional; verify the bench target uses Criterion with harness = false." >&2
            if [ -s "${SCENARIOS_JSON_TMPDIR}/${_scenario_id}.err" ]; then
                echo "  stderr:" >&2
                sed 's/^/    /' "${SCENARIOS_JSON_TMPDIR}/${_scenario_id}.err" >&2
            fi
            OVERALL_OK=0
            continue
        fi

        while IFS= read -r _kv; do
            [ -n "$_kv" ] || continue
            _sid="${_kv%%=*}"
            scenario_selected "$_sid" || [ -z "$SELECTED_SCENARIOS" ] || continue
            SCENARIOS_PATHS+=("$_kv")
        done < <(normalize_criterion_reports "$_criterion_bench")
        echo "WORKLOAD_DONE:  ${_scenario_id} (Criterion reports normalized)"
    done
fi

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
    for key, value in payload.get("metrics", {}).items():
        if isinstance(value, (int, float)):
            metrics[key] = value

    scenario = {
        "id": sid,
        "iterations": n,
        "metrics": metrics,
    }
    if "file" in payload:
        scenario["file"] = payload["file"]
    if "source" in payload:
        scenario["source"] = payload["source"]
    if "metadata" in payload:
        scenario["metadata"] = payload["metadata"]
    if "artifacts" in payload:
        scenario["artifacts"] = payload["artifacts"]
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
