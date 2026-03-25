#!/usr/bin/env bash

# Resolve the commands to run based on event context.
#
# If the COMMANDS input is explicitly set (non-empty and not the old v1 default),
# use it as-is. Otherwise, infer from the GitHub event context.
#
# Reads:
#   COMMANDS_INPUT     — raw input from action consumer (may be empty)
#   SCOPE_CONTEXT      — pr | push | cron | manual (set by scope/resolve.sh)
#
# Outputs (GITHUB_ENV + GITHUB_OUTPUT):
#   RESOLVED_COMMANDS  — comma-separated command list

set -euo pipefail

COMMANDS_INPUT="${COMMANDS_INPUT:-}"
SCOPE_CONTEXT="${SCOPE_CONTEXT:-manual}"

if [ -n "${COMMANDS_INPUT}" ]; then
  RESOLVED_COMMANDS="${COMMANDS_INPUT}"
  echo "Commands from input: ${RESOLVED_COMMANDS}"
else
  # Context-aware defaults
  case "${SCOPE_CONTEXT}" in
    pr)
      RESOLVED_COMMANDS="audit,lint,test"
      ;;
    push)
      RESOLVED_COMMANDS="audit,lint,test"
      ;;
    cron)
      RESOLVED_COMMANDS="release"
      ;;
    manual)
      RESOLVED_COMMANDS="audit,lint,test"
      ;;
    *)
      RESOLVED_COMMANDS="audit,lint,test"
      ;;
  esac
  echo "Commands inferred from context (${SCOPE_CONTEXT}): ${RESOLVED_COMMANDS}"
fi

# Detect refactor-only command sets (e.g., "refactor --from all").
# When all commands are refactor variants, the rerun after autofix is circular
# because the autofix command IS the same refactor — rerunning it is pointless.
REFACTOR_ONLY="false"
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

echo "RESOLVED_COMMANDS=${RESOLVED_COMMANDS}" >> "${GITHUB_ENV}"
echo "resolved-commands=${RESOLVED_COMMANDS}" >> "${GITHUB_OUTPUT}"
echo "refactor-only=${REFACTOR_ONLY}" >> "${GITHUB_OUTPUT}"
