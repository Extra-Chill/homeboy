#!/usr/bin/env bash

# Resolve the commands to run based on event context.
#
# If the COMMANDS input is explicitly set (non-empty and not the old v1 default),
# use it as-is. Otherwise, infer from the GitHub event context.
#
# Commands are split into three categories:
#   1. Quality commands: audit, lint, test, refactor
#      These run in canonical order with component/workspace/scope handling.
#   2. Release command: release
#      This is handled by the dedicated release workflow step.
#   3. Operations commands: fleet, deploy
#      These are passthrough commands that talk to remote servers via SSH.
#      They're never auto-inferred — must be explicitly specified.
#
# Reads:
#   COMMANDS_INPUT     — raw input from action consumer (may be empty)
#   SCOPE_CONTEXT      — pr | push | cron | manual (set by scope/resolve.sh)
#
# Outputs (GITHUB_ENV + GITHUB_OUTPUT):
#   RESOLVED_COMMANDS     — comma-separated quality command list
#   RELEASE_COMMANDS      — comma-separated release command list
#   OPERATIONS_COMMANDS   — comma-separated operations command list (fleet/deploy)

set -euo pipefail

COMMANDS_INPUT="${COMMANDS_INPUT:-}"
SCOPE_CONTEXT="${SCOPE_CONTEXT:-manual}"

if [ -n "${COMMANDS_INPUT}" ]; then
  ALL_COMMANDS="${COMMANDS_INPUT}"
  echo "Commands from input: ${ALL_COMMANDS}"
else
  # Context-aware defaults (never include operations commands)
  case "${SCOPE_CONTEXT}" in
    pr)
      ALL_COMMANDS="audit,lint,test"
      ;;
    push)
      ALL_COMMANDS="audit,lint,test"
      ;;
    cron)
      ALL_COMMANDS="release"
      ;;
    manual)
      ALL_COMMANDS="audit,lint,test"
      ;;
    *)
      ALL_COMMANDS="audit,lint,test"
      ;;
  esac
  echo "Commands inferred from context (${SCOPE_CONTEXT}): ${ALL_COMMANDS}"
fi

# Split commands into quality, release, and operations categories. Release and
# operations commands have dedicated steps; only quality commands enter the
# structured quality-results path.
QUALITY_COMMANDS=()
RELEASE_COMMANDS=()
OPS_COMMANDS=()

IFS=',' read -ra _ALL_ARRAY <<< "${ALL_COMMANDS}"
for _cmd in "${_ALL_ARRAY[@]}"; do
  _cmd="$(echo "${_cmd}" | xargs)"
  [ -z "${_cmd}" ] && continue
  _base=$(echo "${_cmd}" | awk '{print $1}')
  case "${_base}" in
    release)
      RELEASE_COMMANDS+=("${_cmd}")
      ;;
    fleet|deploy)
      OPS_COMMANDS+=("${_cmd}")
      ;;
    *)
      QUALITY_COMMANDS+=("${_cmd}")
      ;;
  esac
done

# Join arrays back to comma-separated strings
RESOLVED_COMMANDS=""
if [ ${#QUALITY_COMMANDS[@]} -gt 0 ]; then
  RESOLVED_COMMANDS="$(IFS=','; printf '%s' "${QUALITY_COMMANDS[*]}")"
fi

OPERATIONS_COMMANDS=""
if [ ${#OPS_COMMANDS[@]} -gt 0 ]; then
  OPERATIONS_COMMANDS="$(IFS=','; printf '%s' "${OPS_COMMANDS[*]}")"
fi

RELEASE_COMMANDS_OUTPUT=""
if [ ${#RELEASE_COMMANDS[@]} -gt 0 ]; then
  RELEASE_COMMANDS_OUTPUT="$(IFS=','; printf '%s' "${RELEASE_COMMANDS[*]}")"
fi

if [ -n "${RESOLVED_COMMANDS}" ]; then
  echo "Quality commands: ${RESOLVED_COMMANDS}"
fi
if [ -n "${OPERATIONS_COMMANDS}" ]; then
  echo "Operations commands: ${OPERATIONS_COMMANDS}"
fi
if [ -n "${RELEASE_COMMANDS_OUTPUT}" ]; then
  echo "Release commands: ${RELEASE_COMMANDS_OUTPUT}"
fi

# Detect refactor-only command sets (e.g., "refactor --from all").
# When all commands are refactor variants, the rerun after autofix is circular
# because the autofix command IS the same refactor — rerunning it is pointless.
REFACTOR_ONLY="false"
if [ -n "${RESOLVED_COMMANDS}" ]; then
  IFS=',' read -ra _CHECK_ARRAY <<< "${RESOLVED_COMMANDS}"
  _ALL_REFACTOR=true
  for _cmd in "${_CHECK_ARRAY[@]}"; do
    _base=$(echo "${_cmd}" | xargs | awk '{print $1}')
    if [ "${_base}" != "refactor" ]; then
      _ALL_REFACTOR=false
      break
    fi
  done
  if [ "${_ALL_REFACTOR}" = true ]; then
    REFACTOR_ONLY="true"
    echo "Commands are refactor-only — rerun after autofix will be skipped"
  fi
fi

echo "RESOLVED_COMMANDS=${RESOLVED_COMMANDS}" >> "${GITHUB_ENV}"
echo "resolved-commands=${RESOLVED_COMMANDS}" >> "${GITHUB_OUTPUT}"
echo "refactor-only=${REFACTOR_ONLY}" >> "${GITHUB_OUTPUT}"

echo "RELEASE_COMMANDS=${RELEASE_COMMANDS_OUTPUT}" >> "${GITHUB_ENV}"
echo "release-commands=${RELEASE_COMMANDS_OUTPUT}" >> "${GITHUB_OUTPUT}"

echo "OPERATIONS_COMMANDS=${OPERATIONS_COMMANDS}" >> "${GITHUB_ENV}"
echo "operations-commands=${OPERATIONS_COMMANDS}" >> "${GITHUB_OUTPUT}"
