#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   extract-engine-data.sh --results results.json --scenario scenario-id \
#       --field key=metadata.engine_data.path --required-field key

error() { echo "ERROR: $*" >&2; exit 1; }
warn() { echo "WARNING: $*" >&2; }

RESULTS_PATH=""
SCENARIO_ID=""
REQUIRED_STATUS="completed"
PRINT_ALIGNED_WIDTH="32"
TO_STDOUT=1
TO_GITHUB_OUTPUT=0
GITHUB_OUTPUT_REQUESTED=0
QUIET=0
FIELDS=()
REQUIRED_FIELDS=()

[ -z "${GITHUB_OUTPUT:-}" ] || TO_GITHUB_OUTPUT=1

next_value() {
    [ "$2" -gt 0 ] || error "$1 requires a value"
    printf '%s' "${3:-}"
}

while [ "$#" -gt 0 ]; do
    arg="$1"
    shift
    case "$arg" in
        --results) RESULTS_PATH=$(next_value --results "$#" "${1:-}"); shift ;;
        --results=*) RESULTS_PATH="${arg#--results=}" ;;
        --scenario) SCENARIO_ID=$(next_value --scenario "$#" "${1:-}"); shift ;;
        --scenario=*) SCENARIO_ID="${arg#--scenario=}" ;;
        --field) FIELDS+=("$(next_value --field "$#" "${1:-}")"); shift ;;
        --field=*) FIELDS+=("${arg#--field=}") ;;
        --required-status) REQUIRED_STATUS=$(next_value --required-status "$#" "${1:-}"); shift ;;
        --required-status=*) REQUIRED_STATUS="${arg#--required-status=}" ;;
        --required-field) REQUIRED_FIELDS+=("$(next_value --required-field "$#" "${1:-}")"); shift ;;
        --required-field=*) REQUIRED_FIELDS+=("${arg#--required-field=}") ;;
        --to-github-output) TO_GITHUB_OUTPUT=1; GITHUB_OUTPUT_REQUESTED=1 ;;
        --to-stdout) TO_STDOUT=1 ;;
        --print-aligned-width) PRINT_ALIGNED_WIDTH=$(next_value --print-aligned-width "$#" "${1:-}"); shift ;;
        --print-aligned-width=*) PRINT_ALIGNED_WIDTH="${arg#--print-aligned-width=}" ;;
        --quiet) QUIET=1 ;;
        *) error "unknown argument: $arg" ;;
    esac
done

[ -n "$RESULTS_PATH" ] || error "--results is required"
[ -s "$RESULTS_PATH" ] || error "results file missing or empty: $RESULTS_PATH"
[ -n "$SCENARIO_ID" ] || error "--scenario is required"
[ "${#FIELDS[@]}" -gt 0 ] || error "at least one --field is required"
[[ "$PRINT_ALIGNED_WIDTH" =~ ^[0-9]+$ ]] || error "--print-aligned-width must be a number"
command -v jq >/dev/null 2>&1 || error "jq required"

if [ "$TO_GITHUB_OUTPUT" = "1" ] && [ -z "${GITHUB_OUTPUT:-}" ]; then
    [ "$GITHUB_OUTPUT_REQUESTED" = "0" ] || warn "--to-github-output requested but GITHUB_OUTPUT is unset; skipping GitHub output"
    TO_GITHUB_OUTPUT=0
fi

if ! scenario_json=$(jq -ec --arg scenario "$SCENARIO_ID" '.scenarios[] | select(.id == $scenario)' "$RESULTS_PATH"); then
    error "scenario not found in results: $SCENARIO_ID"
fi

FIELD_KEYS=()
FIELD_VALUES=()
for field in "${FIELDS[@]}"; do
    [[ "$field" == *=* ]] || error "--field must use key=jq_path, got: $field"
    key="${field%%=*}"
    path="${field#*=}"
    [ -n "$key" ] && [ -n "$path" ] || error "--field requires non-empty key and jq_path, got: $field"
    jq_path="$path"
    [[ "$jq_path" == .* ]] || jq_path=".$jq_path"
    if ! value=$(jq -r "($jq_path) // empty" <<<"$scenario_json"); then
        error "failed to resolve jq path for $key: $path"
    fi

    FIELD_KEYS+=("$key")
    FIELD_VALUES+=("$value")
    [ "$TO_GITHUB_OUTPUT" = "0" ] || printf '%s=%s\n' "$key" "$value" >> "$GITHUB_OUTPUT"
    if [ "$TO_STDOUT" = "1" ] && [ "$QUIET" != "1" ]; then
        printf "%-${PRINT_ALIGNED_WIDTH}s %s\n" "${key}:" "$value"
    fi
done

if [ -n "$REQUIRED_STATUS" ]; then
    actual_status=$(jq -r '.metadata.job_status // empty' <<<"$scenario_json")
    [ "$actual_status" = "$REQUIRED_STATUS" ] || error "scenario $SCENARIO_ID job_status expected $REQUIRED_STATUS, got ${actual_status:-empty}"
fi

for required_key in "${REQUIRED_FIELDS[@]}"; do
    found=0
    for i in "${!FIELD_KEYS[@]}"; do
        [ "${FIELD_KEYS[$i]}" = "$required_key" ] || continue
        found=1
        [ -n "${FIELD_VALUES[$i]}" ] || error "required field $required_key is empty"
    done
    [ "$found" = "1" ] || error "required field was not projected: $required_key"
done
