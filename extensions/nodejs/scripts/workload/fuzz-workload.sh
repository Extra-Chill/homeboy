#!/usr/bin/env bash
set -euo pipefail

# Generic `homeboy workload --kind fuzz` adapter for the Node.js fuzz runner
# (#15840).
#
# Maps the generic HOMEBOY_WORKLOAD_* contract onto the existing
# HOMEBOY_FUZZ_* runner contract, runs scripts/fuzz/fuzz-runner.sh unchanged,
# and projects its homeboy/fuzz-campaign/v1 file into the generic
# homeboy/workload-result/v1 envelope. The campaign rides along as the
# envelope's opaque payload.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESULTS_FILE="${HOMEBOY_WORKLOAD_RESULTS_FILE:?HOMEBOY_WORKLOAD_RESULTS_FILE is required}"
ARTIFACT_DIR="${HOMEBOY_WORKLOAD_ARTIFACT_DIR:?HOMEBOY_WORKLOAD_ARTIFACT_DIR is required}"
CAMPAIGN_FILE="${ARTIFACT_DIR}/fuzz-campaign.json"
FUZZ_ARTIFACT_DIR="${ARTIFACT_DIR}/fuzz"
mkdir -p "$FUZZ_ARTIFACT_DIR" "$(dirname "$RESULTS_FILE")"

export HOMEBOY_FUZZ_RESULTS_FILE="$CAMPAIGN_FILE"
export HOMEBOY_FUZZ_ARTIFACTS_DIR="$FUZZ_ARTIFACT_DIR"
export HOMEBOY_FUZZ_WORKLOAD_ID="${HOMEBOY_WORKLOAD_ID:-}"
export HOMEBOY_FUZZ_RUN_ID="${HOMEBOY_WORKLOAD_RUN_ID:-}"
export HOMEBOY_FUZZ_SEED="${HOMEBOY_WORKLOAD_SEED:-}"
export HOMEBOY_FUZZ_GATE_PROFILE="${HOMEBOY_FUZZ_GATE_PROFILE:-measurement}"
if [ "${HOMEBOY_WORKLOAD_ALLOW_DESTRUCTIVE:-0}" = "1" ]; then
    export HOMEBOY_FUZZ_ALLOW_DESTRUCTIVE=true HOMEBOY_FUZZ_ISOLATION=isolated
else
    export HOMEBOY_FUZZ_ALLOW_DESTRUCTIVE=false HOMEBOY_FUZZ_ISOLATION=shared
fi

runner_exit=0
bash "${SCRIPT_DIR}/../fuzz/fuzz-runner.sh" "$@" || runner_exit=$?

if [ ! -f "$CAMPAIGN_FILE" ]; then
    echo "Error: Node.js fuzz runner wrote no campaign; no workload envelope produced." >&2
    exit "$(( runner_exit == 0 ? 1 : runner_exit ))"
fi

node "${SCRIPT_DIR}/fuzz-envelope.mjs" "$CAMPAIGN_FILE" "$RESULTS_FILE" "$runner_exit" fuzz
exit "$runner_exit"
