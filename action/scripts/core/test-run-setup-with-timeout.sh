#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/core/run-setup-with-timeout.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

run_setup() {
  HOMEBOY_ACTION_SETUP_TIMEOUT_SECONDS=1 \
  HOMEBOY_ACTION_CLEANUP_TIMEOUT_SECONDS=1 \
  HOMEBOY_ACTION_REQUIRE_CONTAINMENT_PROOF=false \
  bash "${RUNNER}" "$@"
}

if [ "$(run_setup 'successful setup' -- bash -c 'printf ready')" != 'ready' ]; then
  printf 'FAIL: successful setup did not preserve output\n'
  exit 1
fi
printf 'PASS: successful setup completes with retained output\n'

set +e
run_setup 'hung child setup' -- bash -c 'sleep 30 & echo $! > "$0"; wait' "${TMP_DIR}/hung-child.pid" >"${TMP_DIR}/hung.log" 2>&1
status=$?
set -e
if [ "${status}" -ne 124 ] || kill -0 "$(<"${TMP_DIR}/hung-child.pid")" 2>/dev/null; then
  printf 'FAIL: hung child setup was not bounded and cleaned up\n'
  exit 1
fi
grep -Fq 'hung child setup timed out during action setup after 1s' "${TMP_DIR}/hung.log" || { printf 'FAIL: hung child lacks actionable evidence\n'; exit 1; }
printf 'PASS: hung child setup times out, reports evidence, and cleans descendants\n'

set +e
run_setup 'stale setup lock' -- bash -c 'mkdir "$0"; while :; do sleep 1; done' "${TMP_DIR}/stale.lock" >"${TMP_DIR}/lock.log" 2>&1
status=$?
set -e
[ "${status}" -eq 124 ] || { printf 'FAIL: stale lock wait was not bounded\n'; exit 1; }
grep -Fq 'stale setup lock exceeded its 1s execution timeout' "${TMP_DIR}/lock.log" || { printf 'FAIL: stale lock lacks timeout evidence\n'; exit 1; }
printf 'PASS: stale lock wait terminates with actionable evidence\n'

set +e
run_setup 'network download' -- bash -c 'sleep 30' >"${TMP_DIR}/network.log" 2>&1
status=$?
set -e
[ "${status}" -eq 124 ] || { printf 'FAIL: network stall was not bounded\n'; exit 1; }
grep -Fq 'network download timed out during action setup after 1s' "${TMP_DIR}/network.log" || { printf 'FAIL: network stall lacks setup evidence\n'; exit 1; }
printf 'PASS: network stall terminates with actionable evidence\n'

set +e
run_setup 'cancelled setup' -- bash -c 'exit 130' >"${TMP_DIR}/cancel.log" 2>&1
status=$?
set -e
[ "${status}" -eq 130 ] || { printf 'FAIL: cancellation exit was masked as %s\n' "${status}"; exit 1; }
printf 'PASS: cancellation exit is preserved\n'
