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
SHARED_LIB_DIR="${HOMEBOY_SHARED_LIB_DIR:-}"
if [ -z "$SHARED_LIB_DIR" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
    SHARED_LIB_DIR="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
fi
SHARED_LIB_DIR="${SHARED_LIB_DIR:-$(cd "${SCRIPT_DIR}/../../../scripts/lib" && pwd)}"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/runner-harness.sh"

# --bash 4 replaces the bash-preflight source, resolve-context runs inside the
# prelude, and --failure-trap carries the same FAILED_STEP/FAILURE_OUTPUT
# fallback this runner used to spell out by hand.
homeboy_runner_harness_init --bash 4 --failure-trap

SETTINGS_HELPER="$(homeboy_runner_harness_resolve_helper HOMEBOY_RUNTIME_SETTINGS_HELPER settings.sh)" || exit 1
BENCH_HELPER_SH="$(homeboy_runner_harness_resolve_helper HOMEBOY_RUNTIME_BENCH_HELPER_SH bench-helper.sh)" || exit 1
TOOLCHAIN_ENV_HELPER="${HOMEBOY_RUST_TOOLCHAIN_ENV_HELPER:-${SCRIPT_DIR}/../lib/toolchain-env.sh}"
# shellcheck source=/dev/null
source "$SETTINGS_HELPER"
# shellcheck source=../lib/toolchain-env.sh
source "$TOOLCHAIN_ENV_HELPER"
# shellcheck source=/dev/null
source "$BENCH_HELPER_SH"

ITERATIONS="${HOMEBOY_BENCH_ITERATIONS:-10}"
RESULTS_FILE="${HOMEBOY_BENCH_RESULTS_FILE:-${PROJECT_PATH}/.rust-bench-results.json}"
LIST_ONLY="${HOMEBOY_BENCH_LIST_ONLY:-0}"
SELECTED_SCENARIOS="${HOMEBOY_BENCH_SCENARIOS:-}"
CRITERION_REQUEST="${HOMEBOY_RUST_BENCH_CRITERION:-0}"
PROFILE_REQUEST="${HOMEBOY_RUST_BENCH_PROFILES:-0}"
ARTIFACT_DIR="${HOMEBOY_BENCH_RESULTS_ARTIFACT_DIR:-$(dirname "$RESULTS_FILE")}"
TOOLCHAIN_METADATA_JSON="$(homeboy_rust_toolchain_metadata_json)"
export HOMEBOY_RUST_TOOLCHAIN_METADATA_JSON="$TOOLCHAIN_METADATA_JSON"

now_ms() {
    python3 - <<'PYTHON_NOW' 2>/dev/null || printf '%s000\n' "$(date +%s)"
import time
print(time.time_ns() // 1_000_000)
PYTHON_NOW
}

RUNNER_START_MS="$(now_ms)"
PHASE_RECORDS=()

record_phase() {
    local _name="$1"
    local _start_ms="$2"
    local _end_ms="$3"
    local _relative_start=$(( _start_ms - RUNNER_START_MS ))
    local _duration=$(( _end_ms - _start_ms ))
    [ "$_relative_start" -lt 0 ] && _relative_start=0
    [ "$_duration" -lt 0 ] && _duration=0
    PHASE_RECORDS+=("${_name}=${_relative_start}=${_duration}")
}

rust_cargo_timings_enabled() {
    case "${HOMEBOY_RUST_BENCH_CARGO_TIMINGS:-}" in
        1|true|TRUE|yes|YES|on|ON)
            return 0
            ;;
    esac

    if [ "$(homeboy_setting_bool rust_bench_cargo_timings false '.rust_bench_cargo_timings // .rust.bench.cargo_timings // false')" = "true" ]; then
        return 0
    fi

    return 1
}

copy_latest_cargo_timing_artifact() {
    local _target_dir="${PROJECT_PATH}/target/cargo-timings"
    local _artifact_dir="$1"

    [ -d "$_target_dir" ] || return 1
    python3 - <<'PYTHON_CARGO_TIMING' "$_target_dir" "$_artifact_dir"
import os, shutil, sys

source_dir, artifact_dir = sys.argv[1:3]
html_files = []
for root, _dirs, files in os.walk(source_dir):
    for filename in files:
        if filename.endswith('.html'):
            path = os.path.join(root, filename)
            html_files.append((os.path.getmtime(path), path))

if not html_files:
    sys.exit(1)

_mtime, source = max(html_files)
os.makedirs(artifact_dir, exist_ok=True)
dest = os.path.join(artifact_dir, 'cargo-timing.html')
shutil.copy2(source, dest)
print(dest)
PYTHON_CARGO_TIMING
}

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

DISCOVERY_START_MS="$(now_ms)"
mapfile -t BENCH_BINS < <(discover_bench_bins)
mapfile -t CRITERION_BENCHES < <(discover_criterion_benches)
DISCOVERY_END_MS="$(now_ms)"
record_phase "bench_discovery" "$DISCOVERY_START_MS" "$DISCOVERY_END_MS"

if [ "$LIST_ONLY" = "1" ]; then
    if ! command -v python3 >/dev/null 2>&1; then
        FAILED_STEP="python3 not on PATH"
        FAILURE_OUTPUT="bench list requires python3 to write the discovery envelope"
        exit 1
    fi

    SCENARIO_ARGS=()
    for _bin in "${BENCH_BINS[@]}"; do
        _scenario_id="${_bin#bench-}"
        homeboy_bench_scenario_selected "$_scenario_id" || continue
        _scenario_file="$(bench_bin_file "$_bin")"
        SCENARIO_ARGS+=("${_scenario_id}=${_scenario_file}=bench-binary")
    done

    if profiles_requested; then
        for _profile_id in "${PROFILE_IDS[@]}"; do
            homeboy_bench_scenario_selected "$_profile_id" || continue
            SCENARIO_ARGS+=("${_profile_id}=null=rust-profile")
        done
    fi

    if criterion_requested; then
        for _criterion_bench in "${CRITERION_BENCHES[@]}"; do
            _scenario_id="criterion-${_criterion_bench}"
            homeboy_bench_scenario_selected "$_scenario_id" || continue
            SCENARIO_ARGS+=("${_scenario_id}=benches/${_criterion_bench}.rs=criterion")
        done
    fi

    homeboy_write_bench_scenario_inventory \
        --results-file "$RESULTS_FILE" \
        --component "$COMPONENT_ID" \
        --iterations "$ITERATIONS" \
        "${SCENARIO_ARGS[@]:-}"

    echo "Discovered ${#SCENARIO_ARGS[@]} Rust bench scenarios."
    exit 0
fi

if [ "${#BENCH_BINS[@]}" -eq 0 ] && ! profiles_requested && ! criterion_requested; then
    echo "" >&2
    echo "⚠ No bench binaries found at ${PROJECT_PATH}" >&2
    echo "  Bench binaries must be named 'bench-*' and live at" >&2
    echo "  src/bin/bench-*.rs or be declared as [[bin]] entries in Cargo.toml." >&2
    echo "" >&2
    homeboy_write_empty_bench_results "$COMPONENT_ID" 0 "$RESULTS_FILE"
    exit 0
fi

SCENARIOS_JSON_TMPDIR="$(mktemp -d)"
cleanup_scenarios() {
    rm -rf "$SCENARIOS_JSON_TMPDIR"
    if type homeboy_print_failure_summary >/dev/null 2>&1; then
        homeboy_print_failure_summary
    fi
}
trap cleanup_scenarios EXIT

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
    homeboy_bench_scenario_selected "$_scenario_id" && SELECTED_BENCH_BINS+=("$_bin")
done

CARGO_TIMING_ENABLED=0
CARGO_TIMING_STATUS="disabled"
CARGO_TIMING_NOTE=""
CARGO_TIMING_ARTIFACT=""
if [ "${#SELECTED_BENCH_BINS[@]}" -gt 0 ]; then
    echo ""
    echo "Building bench binaries (release)..."
    BUILD_ARGS=(build --release --manifest-path="${PROJECT_PATH}/Cargo.toml")
    for _bin in "${SELECTED_BENCH_BINS[@]}"; do
        BUILD_ARGS+=(--bin "$_bin")
    done
    BUILD_LOG="${SCENARIOS_JSON_TMPDIR}/cargo-build.log"
    if rust_cargo_timings_enabled; then
        CARGO_TIMING_ENABLED=1
        BUILD_ARGS+=(--timings)
        CARGO_TIMING_STATUS="requested"
    fi

    BUILD_START_MS="$(now_ms)"
    set +e
    cargo "${BUILD_ARGS[@]}" > "$BUILD_LOG" 2>&1
    BUILD_EXIT=$?
    set -e

    if [ "$BUILD_EXIT" -ne 0 ] && [ "$CARGO_TIMING_ENABLED" = "1" ]; then
        if grep -E -- '--timings|unexpected argument|found argument|unstable|requires -Z' "$BUILD_LOG" >/dev/null 2>&1; then
            echo "WARN: Cargo timing capture unsupported by this Cargo; retrying build without --timings." >&2
            CARGO_TIMING_STATUS="unsupported"
            CARGO_TIMING_NOTE="Cargo rejected --timings; build retried without timing capture."
            BUILD_ARGS=()
            BUILD_ARGS=(build --release --manifest-path="${PROJECT_PATH}/Cargo.toml")
            for _bin in "${SELECTED_BENCH_BINS[@]}"; do
                BUILD_ARGS+=(--bin "$_bin")
            done
            set +e
            cargo "${BUILD_ARGS[@]}" > "$BUILD_LOG" 2>&1
            BUILD_EXIT=$?
            set -e
        fi
    fi

    BUILD_END_MS="$(now_ms)"
    record_phase "cargo_build" "$BUILD_START_MS" "$BUILD_END_MS"

    if [ "$BUILD_EXIT" -ne 0 ]; then
        FAILED_STEP="cargo build"
        tail -20 "$BUILD_LOG" >&2 || true
        exit 1
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        cat "$BUILD_LOG" >&2
    else
        tail -5 "$BUILD_LOG" || true
    fi

    if [ "$CARGO_TIMING_ENABLED" = "1" ] && [ "$CARGO_TIMING_STATUS" != "unsupported" ]; then
        if CARGO_TIMING_ARTIFACT="$(copy_latest_cargo_timing_artifact "$ARTIFACT_DIR" 2>/dev/null)"; then
            CARGO_TIMING_STATUS="captured"
            CARGO_TIMING_NOTE="Cargo timing HTML captured from target/cargo-timings."
        else
            CARGO_TIMING_STATUS="missing"
            CARGO_TIMING_NOTE="Cargo accepted --timings but no HTML timing artifact was found."
        fi
    fi
fi

OVERALL_OK=1
SCENARIOS_PATHS=()
for _bin in "${SELECTED_BENCH_BINS[@]}"; do
    # scenario id = bin name minus "bench-" prefix
    _scenario_id="${_bin#bench-}"
    _scenario_file="${SCENARIOS_JSON_TMPDIR}/${_scenario_id}.json"

    echo ""
    echo "WORKLOAD_BEGIN: ${_scenario_id} (${_bin})"

    _workload_start_ms="$(now_ms)"
    set +e
    HOMEBOY_BENCH_ITERATIONS="$ITERATIONS" \
        cargo run --release --quiet --manifest-path="${PROJECT_PATH}/Cargo.toml" \
        --bin "$_bin" > "${_scenario_file}.raw" 2> "${_scenario_file}.err"
    _exit=$?
    set -e
    _workload_end_ms="$(now_ms)"
    record_phase "workload:${_scenario_id}" "$_workload_start_ms" "$_workload_end_ms"

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
toolchain_metadata = json.loads(os.environ.get('HOMEBOY_RUST_TOOLCHAIN_METADATA_JSON', '{}'))
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
    if toolchain_metadata:
        metadata.setdefault('rust_toolchain', toolchain_metadata)
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

    homeboy_bench_scenario_selected "$_scenario_id" || return 0

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
            homeboy_bench_scenario_selected "$_sid" || [ -z "$SELECTED_SCENARIOS" ] || continue
            SCENARIOS_PATHS+=("$_kv")
        done < <(normalize_criterion_reports "$_criterion_bench")
        echo "WORKLOAD_DONE:  ${_scenario_id} (Criterion reports normalized)"
    done
fi

# ── Build the BenchResults envelope ────────────────────────────────
#
# Core owns the generic BenchResults scenario shape and timing metric math.
# The Rust runner only prepares Rust-specific envelope metadata and artifacts.

if ! command -v python3 >/dev/null 2>&1; then
    FAILED_STEP="python3 not on PATH"
    FAILURE_OUTPUT="bench-runner.sh requires python3 for bench metadata preparation"
    exit 1
fi

PARSE_START_MS="$(now_ms)"
BENCH_EXTRAS_FILE="${SCENARIOS_JSON_TMPDIR}/bench-extras.json"
python3 - <<PYTHON_EXTRAS "$BENCH_EXTRAS_FILE" "$CARGO_TIMING_STATUS" "$CARGO_TIMING_NOTE" "$CARGO_TIMING_ARTIFACT" "$TOOLCHAIN_METADATA_JSON" --phases "${PHASE_RECORDS[@]:-}"
import json, os, sys

extras_file = sys.argv[1]
cargo_timing_status = sys.argv[2]
cargo_timing_note = sys.argv[3]
cargo_timing_artifact = sys.argv[4]
toolchain_metadata = json.loads(sys.argv[5])

args = sys.argv[6:]
phase_records = []
mode = None
for arg in args:
    if arg == "--phases":
        mode = "phases"
        continue
    if mode == "phases" and arg:
        phase_records.append(arg)

timeline = []
phase_metrics = {}
for record in phase_records:
    parts = record.split("=", 2)
    if len(parts) != 3:
        continue
    name, start_ms, duration_ms = parts
    try:
        start_value = int(start_ms)
        duration_value = int(duration_ms)
    except ValueError:
        continue
    timeline.append({
        "id": name.replace(":", "_"),
        "name": name,
        "start_ms": start_value,
        "duration_ms": duration_value,
    })
    metric_key = f"{name.replace(':', '_')}_ms"
    phase_metrics[metric_key] = duration_value

metadata = {
    "rust_runner": {
        "cargo_timing_status": cargo_timing_status,
    },
    "rust_toolchain": toolchain_metadata,
}
artifacts = {}
if cargo_timing_note:
    metadata["rust_runner"]["cargo_timing_note"] = cargo_timing_note
if cargo_timing_artifact:
    artifacts["cargo_timing"] = {
        "path": cargo_timing_artifact,
        "kind": "html",
        "label": "Cargo build timing report",
    }

extras = {
    "metadata":     metadata,
    "metric_groups": {
        "rust_runner_phases_ms": phase_metrics,
    },
    "span_definitions": {
        "rust_runner_phase": {
            "description": "Rust extension runner phases measured outside workload timings.",
            "unit": "ms",
        }
    },
    "timeline": timeline,
}
if artifacts:
    extras["artifacts"] = artifacts

with open(extras_file, "w", encoding="utf-8") as f:
    json.dump(extras, f, indent=2)
PYTHON_EXTRAS

python3 - <<PYTHON_SCENARIO_METADATA --phases "${PHASE_RECORDS[@]:-}" --scenarios "${SCENARIOS_PATHS[@]:-}"
import json, sys

args = sys.argv[1:]
phase_records = []
scenario_kvs = []
mode = None
for arg in args:
    if arg == "--phases":
        mode = "phases"
        continue
    if arg == "--scenarios":
        mode = "scenarios"
        continue
    if mode == "phases" and arg:
        phase_records.append(arg)
    elif mode == "scenarios" and arg:
        scenario_kvs.append(arg)

workload_phase_ms = {}
for record in phase_records:
    parts = record.split("=", 2)
    if len(parts) != 3:
        continue
    name, _start_ms, duration_ms = parts
    if not name.startswith("workload:"):
        continue
    try:
        workload_phase_ms[name.split(":", 1)[1]] = int(duration_ms)
    except ValueError:
        continue

for kv in scenario_kvs:
    if not kv:
        continue
    scenario_id, payload_path = kv.split("=", 1)
    phase_ms = workload_phase_ms.get(scenario_id)
    if phase_ms is None:
        continue
    with open(payload_path, encoding="utf-8") as handle:
        payload = json.load(handle)
    metadata = payload.setdefault("metadata", {})
    rust_runner = metadata.setdefault("rust_runner", {})
    rust_runner.setdefault("workload_run_ms", phase_ms)
    with open(payload_path, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2)
PYTHON_SCENARIO_METADATA

homeboy_write_bench_results_from_payload_files \
    --results-file "$RESULTS_FILE" \
    --component "$COMPONENT_ID" \
    --iterations "$ITERATIONS" \
    --extras-file "$BENCH_EXTRAS_FILE" \
    "${SCENARIOS_PATHS[@]:-}"

printf '\nResults: %s scenario(s) written to %s\n' "${#SCENARIOS_PATHS[@]}" "$RESULTS_FILE"
PARSE_END_MS="$(now_ms)"
RESULT_PARSE_MS=$(( PARSE_END_MS - PARSE_START_MS ))
[ "$RESULT_PARSE_MS" -lt 0 ] && RESULT_PARSE_MS=0
RESULT_PARSE_START_REL=$(( PARSE_START_MS - RUNNER_START_MS ))
[ "$RESULT_PARSE_START_REL" -lt 0 ] && RESULT_PARSE_START_REL=0

python3 - <<'PYTHON_PARSE_PHASE' "$RESULTS_FILE" "$RESULT_PARSE_START_REL" "$RESULT_PARSE_MS"
import json, sys

results_file, start_ms, duration_ms = sys.argv[1:4]
with open(results_file, encoding="utf-8") as fh:
    envelope = json.load(fh)

duration_value = int(duration_ms)
timeline = envelope.setdefault("timeline", [])
timeline.append({
    "id": "result_parse",
    "name": "result_parse",
    "start_ms": int(start_ms),
    "duration_ms": duration_value,
})
metric_groups = envelope.setdefault("metric_groups", {})
phase_metrics = metric_groups.setdefault("rust_runner_phases_ms", {})
phase_metrics["result_parse_ms"] = duration_value

with open(results_file, "w", encoding="utf-8") as fh:
    json.dump(envelope, fh, indent=2)
PYTHON_PARSE_PHASE

if [ "$OVERALL_OK" -ne 1 ]; then
    FAILED_STEP="one or more bench binaries failed"
    exit 1
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [bench:rust] envelope written to $RESULTS_FILE" >&2
fi
