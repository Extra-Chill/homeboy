#!/usr/bin/env bash

# Bound setup subprocesses while retaining their output for the owning CI step.
set -euo pipefail

label="${1:?setup step label is required}"
shift
[ "${1:-}" = '--' ] && shift
[ "$#" -gt 0 ] || { printf 'setup step command is required\n' >&2; exit 2; }

timeout_seconds="${HOMEBOY_ACTION_SETUP_TIMEOUT_SECONDS:-900}"
cleanup_seconds="${HOMEBOY_ACTION_CLEANUP_TIMEOUT_SECONDS:-15}"
log_file="$(mktemp)"
trap 'rm -f "${log_file}"' EXIT

set +e
HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS="${timeout_seconds}" \
HOMEBOY_ACTION_CLEANUP_TIMEOUT_SECONDS="${cleanup_seconds}" \
bash "$(dirname "${BASH_SOURCE[0]}")/run-with-liveness-timeout.sh" \
  --log-file "${log_file}" "${label}" "$@"
status=$?
set -e

cat "${log_file}"
if [ "${status}" -eq 124 ]; then
  echo "::error::${label} timed out during action setup after ${timeout_seconds}s. The retained output above identifies the blocked setup command; retry only after resolving that dependency."
fi
exit "${status}"
