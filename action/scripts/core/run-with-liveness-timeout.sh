#!/usr/bin/env bash

set -euo pipefail

log_file=""
if [ "${1:-}" = "--log-file" ]; then
  log_file="${2:-}"
  shift 2
fi

label="$1"
shift
timeout_seconds="${HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS:-1800}"
cleanup_timeout_seconds="${HOMEBOY_ACTION_CLEANUP_TIMEOUT_SECONDS:-15}"

for value_name in timeout_seconds cleanup_timeout_seconds; do
  value="${!value_name}"
  if ! [[ "${value}" =~ ^[1-9][0-9]*$ ]]; then
    echo "::error::${label} ${value_name//_/ } must be a positive number of seconds; received '${value}'."
    exit 2
  fi
done

start_seconds="$(date +%s)"
timeout_marker="$(mktemp)"
rm -f "${timeout_marker}"
trap 'rm -f "${timeout_marker}"' EXIT

# Job control gives the child command its own process group. Capturing its output
# directly in a file prevents a descendant-held stdout pipe from wedging `tee`
# after the command parent exits.
set -m
if [ -n "${log_file}" ]; then
  "$@" >"${log_file}" 2>&1 &
else
  "$@" &
fi
command_pid=$!
set +m

(
  while kill -0 "${command_pid}" 2>/dev/null; do
    sleep 1
    if kill -0 "${command_pid}" 2>/dev/null; then
      elapsed="$(( $(date +%s) - start_seconds ))"
      if [ "${elapsed}" -ge "${timeout_seconds}" ]; then
        echo "::error::${label} exceeded its ${timeout_seconds}s execution timeout; terminating its process group."
        touch "${timeout_marker}"
        kill -TERM -- "-${command_pid}" 2>/dev/null || true
        cleanup_deadline="$(( $(date +%s) + cleanup_timeout_seconds ))"
        while kill -0 -- "-${command_pid}" 2>/dev/null && [ "$(date +%s)" -lt "${cleanup_deadline}" ]; do
          sleep 1
        done
        if kill -0 -- "-${command_pid}" 2>/dev/null; then
          echo "::warning::${label} ignored SIGTERM for ${cleanup_timeout_seconds}s; sending SIGKILL to process group ${command_pid}."
          kill -KILL -- "-${command_pid}" 2>/dev/null || true
        fi
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

if [ -f "${timeout_marker}" ]; then
  echo "::error::${label} exceeded its ${timeout_seconds}s execution timeout and was terminated. Inspect this step's logs and rerun after resolving the blocked command; the caller can continue when this action is advisory."
  exit 124
fi

if kill -0 -- "-${command_pid}" 2>/dev/null; then
  echo "::warning::${label} finalization found a live process group after the command parent exited; terminating it within ${cleanup_timeout_seconds}s."
  kill -TERM -- "-${command_pid}" 2>/dev/null || true
  cleanup_deadline="$(( $(date +%s) + cleanup_timeout_seconds ))"
  while kill -0 -- "-${command_pid}" 2>/dev/null && [ "$(date +%s)" -lt "${cleanup_deadline}" ]; do
    sleep 1
  done

  if kill -0 -- "-${command_pid}" 2>/dev/null; then
    echo "::warning::${label} finalization grace expired; sending SIGKILL to process group ${command_pid}."
    kill -KILL -- "-${command_pid}" 2>/dev/null || true
    cleanup_deadline="$(( $(date +%s) + cleanup_timeout_seconds ))"
    while kill -0 -- "-${command_pid}" 2>/dev/null && [ "$(date +%s)" -lt "${cleanup_deadline}" ]; do
      sleep 1
    done
  fi

  if kill -0 -- "-${command_pid}" 2>/dev/null; then
    echo "::error::${label} finalization could not terminate process group ${command_pid} within ${cleanup_timeout_seconds}s. Retained command output: ${log_file:-standard output}."
    exit 125
  fi

  echo "::notice::${label} finalization terminated surviving process group ${command_pid}; retained command output: ${log_file:-standard output}."
fi

exit "${command_exit}"
