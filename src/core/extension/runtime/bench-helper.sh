#!/usr/bin/env bash

# Shared BenchResults helpers for shell extension runners.

# scenario slug helper: turn a workload basename into a stable BenchScenario id.
homeboy_bench_scenario_id() {
    local name
    name="$(basename "${1:-}")"
    name="${name%.*}"
    printf '%s' "$name" \
        | sed -E 's/([a-z0-9])([A-Z])/\1-\2/g; s/[^A-Za-z0-9]+/-/g; s/^-+//; s/-+$//' \
        | tr '[:upper:]' '[:lower:]'
}

homeboy_bench_scenario_selected() {
    local scenario="${1:-}"
    local selected="${2:-${HOMEBOY_BENCH_SCENARIOS:-}}"

    [ -n "$scenario" ] || return 1
    [ -z "$selected" ] && return 0
    case ",${selected}," in
        *",${scenario},"*) return 0 ;;
        *) return 1 ;;
    esac
}

homeboy_bench_artifact_ref_json() {
    local path="${1:-}"
    local kind="${2:-}"
    local label="${3:-}"

    if [ -z "$path" ]; then
        echo "homeboy_bench_artifact_ref_json: path is required" >&2
        return 2
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        echo "homeboy_bench_artifact_ref_json: python3 is required" >&2
        return 2
    fi

    HOMEBOY_BENCH_ARTIFACT_PATH="$path" \
    HOMEBOY_BENCH_ARTIFACT_KIND="$kind" \
    HOMEBOY_BENCH_ARTIFACT_LABEL="$label" \
    python3 - <<'PYTHON_BENCH_ARTIFACT_REF'
import json
import os

ref = {'path': os.environ['HOMEBOY_BENCH_ARTIFACT_PATH']}
kind = os.environ.get('HOMEBOY_BENCH_ARTIFACT_KIND') or ''
label = os.environ.get('HOMEBOY_BENCH_ARTIFACT_LABEL') or ''
if kind:
    ref['kind'] = kind
if label:
    ref['label'] = label
print(json.dumps(ref, separators=(',', ':')))
PYTHON_BENCH_ARTIFACT_REF
}

homeboy_bench_responsiveness_ping() {
    local label="${1:-}"
    local file="${HOMEBOY_BENCH_RESPONSIVENESS_FILE:-}"

    [ -n "$file" ] || return 0
    if ! command -v python3 >/dev/null 2>&1; then
        echo "homeboy_bench_responsiveness_ping: python3 is required" >&2
        return 2
    fi

    if [ -z "${__homeboy_bench_responsiveness_started_ms:-}" ]; then
        __homeboy_bench_responsiveness_started_ms="$(python3 - <<'PYTHON_BENCH_RESPONSIVENESS_START'
from datetime import datetime, timezone
print(int(datetime.now(timezone.utc).timestamp() * 1000))
PYTHON_BENCH_RESPONSIVENESS_START
)"
    fi

    mkdir -p "$(dirname "$file")"
    HOMEBOY_BENCH_RESPONSIVENESS_STARTED_MS="$__homeboy_bench_responsiveness_started_ms" \
    HOMEBOY_BENCH_RESPONSIVENESS_LABEL="$label" \
    python3 - "$file" <<'PYTHON_BENCH_RESPONSIVENESS_PING'
import json
import os
import sys
from datetime import datetime, timezone

now_ms = int(datetime.now(timezone.utc).timestamp() * 1000)
started_ms = int(os.environ['HOMEBOY_BENCH_RESPONSIVENESS_STARTED_MS'])
ping = {
    'at': datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
    't_ms': max(0, now_ms - started_ms),
}
label = os.environ.get('HOMEBOY_BENCH_RESPONSIVENESS_LABEL') or ''
if label:
    ping['label'] = label
with open(sys.argv[1], 'a', encoding='utf-8') as handle:
    handle.write(json.dumps(ping, separators=(',', ':')) + '\n')
PYTHON_BENCH_RESPONSIVENESS_PING
}

homeboy_write_empty_bench_results() {
    local component_id="${1:-${HOMEBOY_COMPONENT_ID:-}}"
    local iterations="${2:-0}"
    local results_file="${3:-${HOMEBOY_BENCH_RESULTS_FILE:-}}"

    if [ -z "$component_id" ]; then
        echo "homeboy_write_empty_bench_results: component id is required" >&2
        return 2
    fi
    if [ -z "$results_file" ]; then
        echo "homeboy_write_empty_bench_results: HOMEBOY_BENCH_RESULTS_FILE is required" >&2
        return 2
    fi

    mkdir -p "$(dirname "$results_file")"
    printf '{"component_id":"%s","iterations":%s,"scenarios":[]}\n' "$component_id" "$iterations" > "$results_file"
}

homeboy_write_bench_scenario_inventory() {
    homeboy_write_bench_results_from_payload_files --inventory "$@"
}

homeboy_write_bench_results_from_payload_files() {
    local inventory=0
    local results_file="${HOMEBOY_BENCH_RESULTS_FILE:-}"
    local component_id="${HOMEBOY_COMPONENT_ID:-}"
    local iterations="${HOMEBOY_BENCH_ITERATIONS:-0}"
    local metadata_json="{}"
    local metric_groups_json="{}"
    local span_definitions_json="{}"
    local artifacts_json="{}"
    local timeline_json="[]"
    local extras_file=""
    local scenario_args=()

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --inventory)
                inventory=1
                shift
                ;;
            --results-file)
                results_file="${2:-}"
                shift 2
                ;;
            --component|--component-id)
                component_id="${2:-}"
                shift 2
                ;;
            --iterations)
                iterations="${2:-0}"
                shift 2
                ;;
            --metadata-json)
                metadata_json="${2:-{}}"
                shift 2
                ;;
            --metric-groups-json)
                metric_groups_json="${2:-{}}"
                shift 2
                ;;
            --span-definitions-json)
                span_definitions_json="${2:-{}}"
                shift 2
                ;;
            --artifacts-json)
                artifacts_json="${2:-{}}"
                shift 2
                ;;
            --timeline-json)
                timeline_json="${2:-[]}"
                shift 2
                ;;
            --extras-file)
                extras_file="${2:-}"
                shift 2
                ;;
            --)
                shift
                while [ "$#" -gt 0 ]; do
                    scenario_args+=("$1")
                    shift
                done
                ;;
            *)
                scenario_args+=("$1")
                shift
                ;;
        esac
    done

    if [ -z "$component_id" ]; then
        echo "homeboy_write_bench_results_from_payload_files: component id is required" >&2
        return 2
    fi
    if [ -z "$results_file" ]; then
        echo "homeboy_write_bench_results_from_payload_files: HOMEBOY_BENCH_RESULTS_FILE is required" >&2
        return 2
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        echo "homeboy_write_bench_results_from_payload_files: python3 is required" >&2
        return 2
    fi

    mkdir -p "$(dirname "$results_file")"
    HOMEBOY_BENCH_INVENTORY="$inventory" \
    HOMEBOY_BENCH_RESULTS_FILE_TARGET="$results_file" \
    HOMEBOY_BENCH_COMPONENT_ID="$component_id" \
    HOMEBOY_BENCH_ITERATIONS_VALUE="$iterations" \
    HOMEBOY_BENCH_METADATA_JSON="$metadata_json" \
    HOMEBOY_BENCH_METRIC_GROUPS_JSON="$metric_groups_json" \
    HOMEBOY_BENCH_SPAN_DEFINITIONS_JSON="$span_definitions_json" \
    HOMEBOY_BENCH_ARTIFACTS_JSON="$artifacts_json" \
    HOMEBOY_BENCH_TIMELINE_JSON="$timeline_json" \
    HOMEBOY_BENCH_EXTRAS_FILE="$extras_file" \
    python3 - "${scenario_args[@]}" <<'PYTHON_BENCH_RESULTS'
import json
import os
import sys


def json_env(name, fallback):
    raw = os.environ.get(name, '')
    if not raw:
        return fallback
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise SystemExit(f'{name} must be valid JSON: {exc}')


# R-7 percentile, the runner contract used by Homeboy BenchResults producers.
def percentile_r7(sorted_values, p):
    n = len(sorted_values)
    if n == 0:
        return 0.0
    if n == 1:
        return float(sorted_values[0])
    rank = p * (n - 1)
    lo = int(rank)
    hi = min(lo + (1 if rank > lo else 0), n - 1)
    if lo == hi:
        return float(sorted_values[lo])
    frac = rank - lo
    return float(sorted_values[lo] * (1 - frac) + sorted_values[hi] * frac)


def split_record(record):
    parts = record.split('=', 3)
    while len(parts) < 4:
        parts.append('')
    return parts[0], parts[1], parts[2], parts[3]


def load_payload(path):
    with open(path, encoding='utf-8') as handle:
        return json.load(handle)


inventory = os.environ.get('HOMEBOY_BENCH_INVENTORY') == '1'
results_file = os.environ['HOMEBOY_BENCH_RESULTS_FILE_TARGET']
component_id = os.environ['HOMEBOY_BENCH_COMPONENT_ID']
iterations = int(os.environ.get('HOMEBOY_BENCH_ITERATIONS_VALUE') or 0)
metadata = json_env('HOMEBOY_BENCH_METADATA_JSON', {})
metric_groups = json_env('HOMEBOY_BENCH_METRIC_GROUPS_JSON', {})
span_definitions = json_env('HOMEBOY_BENCH_SPAN_DEFINITIONS_JSON', {})
artifacts = json_env('HOMEBOY_BENCH_ARTIFACTS_JSON', {})
timeline = json_env('HOMEBOY_BENCH_TIMELINE_JSON', [])
extras_file = os.environ.get('HOMEBOY_BENCH_EXTRAS_FILE', '')
if extras_file:
    with open(extras_file, encoding='utf-8') as handle:
        extras = json.load(handle)
    metadata = extras.get('metadata', metadata)
    metric_groups = extras.get('metric_groups', metric_groups)
    span_definitions = extras.get('span_definitions', span_definitions)
    artifacts = extras.get('artifacts', artifacts)
    timeline = extras.get('timeline', timeline)

scenarios = []
for record in sys.argv[1:]:
    if not record:
        continue
    scenario_id, payload_path, file_value, source = split_record(record)
    if inventory and not source and payload_path and payload_path != 'null' and file_value:
        payload_path, file_value, source = '', payload_path, file_value
    scenario = {'id': scenario_id}

    payload = {}
    if payload_path and payload_path != 'null':
        payload = load_payload(payload_path)

    if inventory:
        scenario['iterations'] = 0
        scenario['default_iterations'] = iterations
        scenario['tags'] = payload.get('tags', [])
        scenario['metrics'] = payload.get('metrics', {})
    else:
        timings_ns = payload.get('timings_ns', [])
        timings_ms = sorted(float(value) / 1_000_000.0 for value in timings_ns)
        count = len(timings_ms)
        if count == 0:
            sys.stderr.write(f'WORKLOAD_WARN: {scenario_id} emitted no timings\n')
            continue
        metrics = {
            'mean_ms': sum(timings_ms) / count,
            'p50_ms': percentile_r7(timings_ms, 0.50),
            'p95_ms': percentile_r7(timings_ms, 0.95),
            'p99_ms': percentile_r7(timings_ms, 0.99),
            'min_ms': timings_ms[0],
            'max_ms': timings_ms[-1],
        }
        for key, value in payload.get('metrics', {}).items():
            if isinstance(value, (int, float)):
                metrics[key] = value
        scenario['iterations'] = count
        scenario['metrics'] = metrics

    if file_value and file_value != 'null':
        scenario['file'] = file_value
    elif isinstance(payload.get('file'), str):
        scenario['file'] = payload['file']
    if source:
        scenario['source'] = source
    elif isinstance(payload.get('source'), str):
        scenario['source'] = payload['source']
    if isinstance(payload.get('metadata'), dict):
        scenario['metadata'] = payload['metadata']
    if isinstance(payload.get('artifacts'), dict):
        scenario['artifacts'] = payload['artifacts']
    if 'peak_rss_bytes' in payload:
        scenario['memory'] = {'peak_bytes': int(payload['peak_rss_bytes'])}

    scenarios.append(scenario)

envelope = {
    'component_id': component_id,
    'iterations': 0 if inventory else iterations,
    'scenarios': scenarios,
}
if metadata:
    envelope['metadata'] = metadata
if metric_groups:
    envelope['metric_groups'] = metric_groups
if span_definitions:
    envelope['span_definitions'] = span_definitions
if artifacts:
    envelope['artifacts'] = artifacts
if timeline:
    envelope['timeline'] = timeline

with open(results_file, 'w', encoding='utf-8') as handle:
    json.dump(envelope, handle, indent=2)
    handle.write('\n')
PYTHON_BENCH_RESULTS
}
