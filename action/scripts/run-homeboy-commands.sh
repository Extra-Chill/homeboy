#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/lib.sh"

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

IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS}"
HAS_LINT_COMMAND="$(has_lint_command "${COMMANDS}")"

for CMD in "${CMD_ARRAY[@]}"; do
  CMD=$(echo "${CMD}" | xargs)

  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Running: homeboy ${CMD} ${COMP_ID}"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  if [ "${CMD}" = "test" ] && [ "${HAS_LINT_COMMAND}" = "true" ]; then
    export HOMEBOY_SKIP_LINT=1
  else
    unset HOMEBOY_SKIP_LINT 2>/dev/null || true
  fi

  if [ "${CMD}" = "audit" ] && [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
    FULL_CMD="homeboy audit ${COMP_ID} --path ${WORKSPACE} --changed-since ${HOMEBOY_CHANGED_SINCE}"
  elif [ "${CMD}" = "audit" ]; then
    FULL_CMD="homeboy audit ${COMP_ID} --path ${WORKSPACE}"
  elif [ "${CMD}" = "lint" ] && [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
    FULL_CMD="homeboy lint ${COMP_ID} --path ${WORKSPACE} --changed-since ${HOMEBOY_CHANGED_SINCE}"
  elif [ "${CMD}" = "test" ] && [ "${TEST_SCOPE:-full}" = "changed" ] && [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
    FULL_CMD="homeboy test ${COMP_ID} --path ${WORKSPACE} --changed-since ${HOMEBOY_CHANGED_SINCE}"
  else
    FULL_CMD="homeboy ${CMD} ${COMP_ID} --path ${WORKSPACE}"
  fi

  if [ -n "${EXTRA_ARGS:-}" ]; then
    FULL_CMD="${FULL_CMD} ${EXTRA_ARGS}"
  fi

  echo "::group::${GROUP_PREFIX} ${CMD}"
  CMD_EXIT=0
  set +e
  eval "${FULL_CMD}" 2>&1 | tee "${HOMEBOY_OUTPUT_DIR}/${CMD}.log"
  CMD_EXIT=${PIPESTATUS[0]}
  set -e
  echo "::endgroup::"

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
