#!/usr/bin/env bash
set -euo pipefail

# Replay one persisted WordPress fuzz case for `homeboy workload replay --kind
# fuzz` (Extra-Chill/homeboy#15840).
#
# The WordPress runner has no single-case entry point, so replay re-runs the
# source run's workload file (HOMEBOY_WORKLOAD_PATH) with the case's seed and
# reports whether that case still fails. Exits 1 while it reproduces, 0 once it
# passes, 2 when the case no longer exists in the replayed campaign.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CASE_ID="${HOMEBOY_WORKLOAD_CASE_ID:?HOMEBOY_WORKLOAD_CASE_ID is required}"
: "${HOMEBOY_WORKLOAD_PATH:?HOMEBOY_WORKLOAD_PATH is required: replay re-runs the workload file of the source run}"

REPLAY_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-fuzz-replay.XXXXXX")"
trap 'rm -rf "$REPLAY_DIR"' EXIT

runner_exit=0
HOMEBOY_WORKLOAD_RESULTS_FILE="${REPLAY_DIR}/envelope.json" \
HOMEBOY_WORKLOAD_ARTIFACT_DIR="${REPLAY_DIR}/artifacts" \
HOMEBOY_WORKLOAD_RUN_ID="${HOMEBOY_WORKLOAD_RUN_ID:-replay}-replay-${CASE_ID}" \
    bash "${SCRIPT_DIR}/fuzz-workload.sh" "$@" >&2 || runner_exit=$?

if [ ! -f "${REPLAY_DIR}/envelope.json" ]; then
    echo "Error: replay of case ${CASE_ID} produced no envelope (runner exit ${runner_exit})." >&2
    exit "$(( runner_exit == 0 ? 1 : runner_exit ))"
fi

node - "${REPLAY_DIR}/envelope.json" "$CASE_ID" <<'NODE'
const fs = require('fs');
const [file, caseId] = process.argv.slice(2);
const envelope = JSON.parse(fs.readFileSync(file, 'utf8'));
const replayed = (envelope.cases || []).find((c) => c.case_id === caseId);
const findings = (envelope.findings || []).filter((f) => f.metadata && f.metadata.case_id === caseId);
console.log(JSON.stringify({ case_id: caseId, seed: process.env.HOMEBOY_WORKLOAD_SEED || null, status: replayed ? replayed.status : 'missing', findings }));
if (!replayed) {
  console.error(`replay: case ${caseId} is not in the replayed campaign`);
  process.exit(2);
}
if (replayed.status === 'fail') {
  console.error(`replay: case ${caseId} still fails`);
  process.exit(1);
}
console.error(`replay: case ${caseId} now ${replayed.status}`);
NODE
