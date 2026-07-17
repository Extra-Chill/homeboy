#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/core/run-with-liveness-timeout.sh"
ACTION="${ROOT_DIR}/action.yml"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

success_log="${TMP_DIR}/success.log"
HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS=2 bash "${RUNNER}" --log-file "${success_log}" "fast command" bash -c 'printf success'
if [ "$(<"${success_log}")" != "success" ]; then
  printf 'FAIL: successful command log was not retained\n'
  exit 1
fi
printf 'PASS: successful command preserves its retained output\n'

set +e
child_pid_file="${TMP_DIR}/child.pid"
HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS=1 HOMEBOY_ACTION_CLEANUP_TIMEOUT_SECONDS=1 bash "${RUNNER}" --log-file "${TMP_DIR}/timeout-command.log" "stalled autofix" bash -c 'sleep 30 & echo $! > "$0"; wait' "${child_pid_file}" >"${TMP_DIR}/timeout.log" 2>&1
exit_code=$?
set -e

if [ "${exit_code}" -ne 124 ]; then
  printf 'FAIL: timeout exits 124, got %s\n' "${exit_code}"
  exit 1
fi

if ! grep -q 'stalled autofix exceeded its 1s execution timeout' "${TMP_DIR}/timeout.log"; then
  printf 'FAIL: timeout reports actionable evidence\n'
  exit 1
fi
printf 'PASS: timeout returns actionable evidence\n'

child_pid="$(<"${child_pid_file}")"
if kill -0 "${child_pid}" 2>/dev/null; then
  printf 'FAIL: timeout leaves descendant process %s alive\n' "${child_pid}"
  exit 1
fi
printf 'PASS: timeout terminates descendant process group\n'

leaked_pid_file="${TMP_DIR}/leaked.pid"
HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS=5 HOMEBOY_ACTION_CLEANUP_TIMEOUT_SECONDS=1 bash "${RUNNER}" --log-file "${TMP_DIR}/leaked-command.log" "leaked child" bash -c 'sleep 30 & echo $! > "$0"; printf retained' "${leaked_pid_file}" >"${TMP_DIR}/cleanup.log" 2>&1
leaked_pid="$(<"${leaked_pid_file}")"
if kill -0 "${leaked_pid}" 2>/dev/null; then
  printf 'FAIL: finalization leaves child process %s alive\n' "${leaked_pid}"
  exit 1
fi
if ! grep -q 'finalization terminated surviving process group' "${TMP_DIR}/cleanup.log"; then
  printf 'FAIL: finalization does not report child-process cleanup\n'
  exit 1
fi
if [ "$(<"${TMP_DIR}/leaked-command.log")" != "retained" ]; then
  printf 'FAIL: child cleanup did not preserve command output\n'
  exit 1
fi
printf 'PASS: finalization cleans children and reports retained output\n'

set +e
HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS=invalid bash "${RUNNER}" "invalid timeout" bash -c 'exit 0' >"${TMP_DIR}/invalid.log" 2>&1
exit_code=$?
set -e

if [ "${exit_code}" -ne 2 ]; then
  printf 'FAIL: invalid timeout exits 2, got %s\n' "${exit_code}"
  exit 1
fi
printf 'PASS: invalid timeout is rejected\n'

if ! grep -q '^  execution-timeout-seconds:' "${ACTION}"; then
  printf 'FAIL: action does not expose execution timeout input\n'
  exit 1
fi
if ! grep -q '^  cleanup-timeout-seconds:' "${ACTION}"; then
  printf 'FAIL: action does not expose cleanup timeout input\n'
  exit 1
fi

wrapper_count="$(grep -c 'run-with-liveness-timeout.sh' "${ACTION}")"
if [ "${wrapper_count}" -ne 1 ]; then
  printf 'FAIL: action must leave command timeout enforcement to the per-command runner, got %s action wrappers\n' "${wrapper_count}"
  exit 1
fi
if ! grep -q 'run-with-liveness-timeout.sh' "${ROOT_DIR}/scripts/core/run-homeboy-commands.sh"; then
  printf 'FAIL: quality commands are not individually bounded\n'
  exit 1
fi
if grep -q 'run-with-liveness-timeout.sh.*| tee' "${ROOT_DIR}/scripts/core/run-homeboy-commands.sh"; then
  printf 'FAIL: quality command finalization still depends on an unbounded tee pipeline\n'
  exit 1
fi
printf 'PASS: action applies bounded execution to each quality command and autofix\n'
