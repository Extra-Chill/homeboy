#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

# v2: prefer RESOLVED_COMMANDS (from resolve-commands.sh), fall back to COMMANDS for compat
EFFECTIVE_COMMANDS="${RESOLVED_COMMANDS:-${COMMANDS:-audit,lint,test}}"

# If no quality commands to run (e.g. operations-only mode), exit cleanly
if [ -z "${EFFECTIVE_COMMANDS}" ]; then
  echo "No quality commands to run"
  echo "results={}" >> "${GITHUB_OUTPUT}"
  exit 0
fi

COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"
RESULTS='{}'
OVERALL_EXIT=0
GROUP_PREFIX="${RUN_GROUP_PREFIX:-homeboy}"

HOMEBOY_OUTPUT_DIR=$(mktemp -d)
echo "HOMEBOY_OUTPUT_DIR=${HOMEBOY_OUTPUT_DIR}" >> "${GITHUB_ENV}"

HOMEBOY_ANNOTATIONS_DIR=$(mktemp -d)
echo "HOMEBOY_ANNOTATIONS_DIR=${HOMEBOY_ANNOTATIONS_DIR}" >> "${GITHUB_ENV}"
export HOMEBOY_ANNOTATIONS_DIR

# Enforce canonical order: audit → lint → test
ORDERED_COMMANDS="$(canonicalize_commands "${EFFECTIVE_COMMANDS}")"
IFS=',' read -ra CMD_ARRAY <<< "${ORDERED_COMMANDS}"
HAS_LINT_COMMAND="$(has_lint_command "${EFFECTIVE_COMMANDS}")"

for CMD in "${CMD_ARRAY[@]}"; do
  CMD=$(echo "${CMD}" | xargs)

  # Release is handled by the dedicated release step, not the command loop
  if [ "${CMD}" = "release" ]; then
    continue
  fi

  if [ "${CMD}" = "test" ] && [ "${HAS_LINT_COMMAND}" = "true" ]; then
    export HOMEBOY_SKIP_LINT=1
  else
    unset HOMEBOY_SKIP_LINT 2>/dev/null || true
  fi

  OUTPUT_STEM="$(command_output_stem "${CMD}")"
  OUTPUT_JSON="${HOMEBOY_OUTPUT_DIR}/${OUTPUT_STEM}.json"
  FULL_CMD="$(build_run_command "${CMD}" "${COMP_ID}" "${WORKSPACE}" "${OUTPUT_JSON}")"

  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Running: ${FULL_CMD}"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  echo "::group::${GROUP_PREFIX} ${CMD}"
  CMD_EXIT=0
  set +e
  eval "${FULL_CMD}" 2>&1 | tee "${HOMEBOY_OUTPUT_DIR}/${OUTPUT_STEM}.log"
  CMD_EXIT=${PIPESTATUS[0]}
  set -e
  echo "::endgroup::"

  if [ ! -s "${OUTPUT_JSON}" ]; then
    echo "::warning::homeboy ${CMD} did not write structured output to ${OUTPUT_JSON}"
  fi

  if [ "${CMD_EXIT}" -eq 0 ]; then
    echo "::notice::homeboy ${CMD}: PASSED"
    RESULTS=$(echo "${RESULTS}" | jq -c --arg cmd "${CMD}" '. + {($cmd): "pass"}')
  else
    echo "::error::homeboy ${CMD}: FAILED (exit code ${CMD_EXIT})"
    RESULTS=$(echo "${RESULTS}" | jq -c --arg cmd "${CMD}" '. + {($cmd): "fail"}')
    OVERALL_EXIT=1
  fi
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Results: ${RESULTS}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

echo "results=${RESULTS}" >> "${GITHUB_OUTPUT}"
exit "${OVERALL_EXIT}"
