#!/usr/bin/env bash

# Pure functions for CLI flag generation based on current scope.
# Source this file — do not execute directly.
# Reads SCOPE_* env vars set by resolve.sh.

# Get CLI flags for a command based on current scope.
# Usage: scope_flags_for "lint"
#        scope_flags_for "refactor --from all --write"
# Prints: "--changed-since abc123" or ""
scope_flags_for() {
  local cmd="$1"
  local base_cmd

  # Extract the base command (first word) from compound commands like "refactor --from all --write"
  base_cmd=$(printf '%s' "${cmd}" | awk '{print $1}')

  if [ "${SCOPE_MODE:-full}" != "changed" ] || [ -z "${SCOPE_BASE_REF:-}" ]; then
    return
  fi

  case "${base_cmd}" in
    audit|lint|test|refactor)
      printf '%s' "--changed-since ${SCOPE_BASE_REF}"
      ;;
    # release and other commands are never scoped
  esac
}
