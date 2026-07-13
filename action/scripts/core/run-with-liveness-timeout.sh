#!/usr/bin/env bash

set -euo pipefail

label="$1"
shift
timeout_seconds="${HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS:-1800}"

if ! [[ "${timeout_seconds}" =~ ^[1-9][0-9]*$ ]]; then
  echo "::error::${label} timeout must be a positive number of seconds; received '${timeout_seconds}'."
  exit 2
fi

start_seconds="$(date +%s)"
if command -v timeout >/dev/null 2>&1; then
  timeout --foreground --signal=TERM --kill-after=30s "${timeout_seconds}" "$@" &
else
  perl -e 'alarm shift; exec @ARGV' "${timeout_seconds}" "$@" &
fi
command_pid=$!

(
  while kill -0 "${command_pid}" 2>/dev/null; do
    sleep 60
    if kill -0 "${command_pid}" 2>/dev/null; then
      elapsed="$(( $(date +%s) - start_seconds ))"
      echo "::notice::${label} is still running after ${elapsed}s (timeout: ${timeout_seconds}s)."
    fi
  done
) &
liveness_pid=$!

set +e
wait "${command_pid}"
command_exit=$?
set -e
kill "${liveness_pid}" 2>/dev/null || true
wait "${liveness_pid}" 2>/dev/null || true

if [ "${command_exit}" -eq 124 ] || [ "${command_exit}" -eq 142 ]; then
  echo "::error::${label} exceeded its ${timeout_seconds}s execution timeout and was terminated. Inspect this step's logs and rerun after resolving the blocked command; the caller can continue when this action is advisory."
  exit 124
fi

exit "${command_exit}"
