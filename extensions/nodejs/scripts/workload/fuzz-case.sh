#!/usr/bin/env bash
set -euo pipefail

# Replay or minimize one persisted fuzz case for `homeboy workload replay|minimize
# --kind fuzz` (#15840).
#
# Runs the component's package.json `fuzz:replay` / `fuzz:minimize` script
# with the generic case env (HOMEBOY_WORKLOAD_CASE_ID, _SEED,
# _CASE_ARTIFACT_FILE, _SOURCE_RESULTS_FILE, ...) and the matching
# HOMEBOY_FUZZ_REPLAY_* names the fuzz runner contract already uses.

MODE="${HOMEBOY_WORKLOAD_CASE_MODE:?HOMEBOY_WORKLOAD_CASE_MODE is required}"
COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
SCRIPT_NAME="fuzz:${MODE}"

cd "$COMPONENT_PATH"
if ! node -e "const p=require('./package.json'); process.exit(p.scripts && p.scripts[process.argv[1]] ? 0 : 1)" "$SCRIPT_NAME"; then
    echo "Error: package.json declares no \`${SCRIPT_NAME}\` script; cannot ${MODE} fuzz case ${HOMEBOY_WORKLOAD_CASE_ID:-}." >&2
    exit 2
fi

export HOMEBOY_FUZZ_REPLAY_CASE_ID="${HOMEBOY_WORKLOAD_CASE_ID:-}"
export HOMEBOY_FUZZ_REPLAY_SEED="${HOMEBOY_WORKLOAD_SEED:-}"
export HOMEBOY_FUZZ_REPLAY_RUN_ID="${HOMEBOY_WORKLOAD_RUN_ID:-}"
export HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE="${HOMEBOY_WORKLOAD_CASE_ARTIFACT_FILE:-}"

if [ -f pnpm-lock.yaml ] && command -v pnpm >/dev/null 2>&1; then
    exec pnpm run --silent "$SCRIPT_NAME" -- "$@"
fi
exec npm run --silent "$SCRIPT_NAME" -- "$@"
