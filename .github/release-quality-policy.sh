#!/usr/bin/env bash

set -euo pipefail

is_blocking_command() {
  local command="$1"
  local configured
  local canonical

  IFS=',' read -r -a configured_commands <<< "${BLOCKING_COMMANDS}"
  for configured in "${configured_commands[@]}"; do
    canonical="$(printf '%s' "${configured}" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
    canonical="${canonical#review}"
    if [ "${canonical}" = "${command}" ]; then
      return 0
    fi
  done

  return 1
}

failed=0

check_command() {
  local command="$1"
  local result="$2"

  if is_blocking_command "${command}"; then
    if [ "${result}" = "success" ]; then
      echo "::notice::Release-blocking command ${command} passed"
    else
      echo "::error::Release-blocking command ${command} finished with result: ${result}"
      failed=1
    fi
  else
    echo "::notice::Command ${command} is tracked but not release-blocking (result: ${result})"
  fi
}

check_command audit "${AUDIT_RESULT}"
check_command lint "${LINT_RESULT}"
check_command test "${TEST_RESULT}"

exit "${failed}"
