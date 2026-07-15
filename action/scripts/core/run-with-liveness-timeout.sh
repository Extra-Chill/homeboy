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

# Job control gives the child command its own process group. Killing only the
# wrapper leaves grandchildren such as cargo and homeboy running after a timeout.
set -m
"$@" &
command_pid=$!
set +m

(
  while kill -0 "${command_pid}" 2>/dev/null; do
    sleep 1
    if kill -0 "${command_pid}" 2>/dev/null; then
      elapsed="$(( $(date +%s) - start_seconds ))"
      if [ "${elapsed}" -ge "${timeout_seconds}" ]; then
        echo "::error::${label} exceeded its ${timeout_seconds}s execution timeout; terminating its process group."
        kill -TERM -- "-${command_pid}" 2>/dev/null || true
        sleep 5
        kill -KILL -- "-${command_pid}" 2>/dev/null || true
        exit 0
      fi
      if [ "$(( elapsed % 60 ))" -eq 0 ]; then
        echo "::notice::${label} is still running after ${elapsed}s (timeout: ${timeout_seconds}s)."
      fi
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

if [ "$(( $(date +%s) - start_seconds ))" -ge "${timeout_seconds}" ]; then
  echo "::error::${label} exceeded its ${timeout_seconds}s execution timeout and was terminated. Inspect this step's logs and rerun after resolving the blocked command; the caller can continue when this action is advisory."
  exit 124
fi

exit "${command_exit}"
