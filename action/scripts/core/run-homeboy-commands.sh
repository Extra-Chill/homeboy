#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

# v2: prefer RESOLVED_COMMANDS (quality-only from resolve-commands.sh), fall back to review-nested defaults.
EFFECTIVE_COMMANDS="${RESOLVED_COMMANDS:-${COMMANDS:-review audit,review lint,review test}}"

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

HOMEBOY_CI_RESULTS_DIR="${GITHUB_WORKSPACE:-$(pwd)}/homeboy-ci-results"
mkdir -p "${HOMEBOY_CI_RESULTS_DIR}"
echo "HOMEBOY_CI_RESULTS_DIR=${HOMEBOY_CI_RESULTS_DIR}" >> "${GITHUB_ENV}"

HOMEBOY_ANNOTATIONS_DIR=$(mktemp -d)
echo "HOMEBOY_ANNOTATIONS_DIR=${HOMEBOY_ANNOTATIONS_DIR}" >> "${GITHUB_ENV}"
export HOMEBOY_ANNOTATIONS_DIR

# Enforce canonical order: review audit → review lint → review test
ORDERED_COMMANDS="$(canonicalize_commands "${EFFECTIVE_COMMANDS}")"
IFS=',' read -ra CMD_ARRAY <<< "${ORDERED_COMMANDS}"
HAS_LINT_COMMAND="$(has_lint_command "${EFFECTIVE_COMMANDS}")"

for CMD in "${CMD_ARRAY[@]}"; do
  CMD=$(echo "${CMD}" | xargs)

  if [ "$(quality_base_command "${CMD}")" = "test" ] && [ "${HAS_LINT_COMMAND}" = "true" ]; then
    export HOMEBOY_SKIP_LINT=1
  else
    unset HOMEBOY_SKIP_LINT 2>/dev/null || true
  fi

  OUTPUT_STEM="$(command_output_stem "${CMD}")"
  if [ "$(printf '%s' "${CMD}" | awk '{print $1}')" = "bench" ]; then
    OUTPUT_JSON="${HOMEBOY_CI_RESULTS_DIR}/bench.json"
  else
    OUTPUT_JSON="${HOMEBOY_OUTPUT_DIR}/${OUTPUT_STEM}.json"
  fi
  FULL_CMD="$(build_run_command "${CMD}" "${COMP_ID}" "${WORKSPACE}" "${OUTPUT_JSON}")"

  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Running: ${FULL_CMD}"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  echo "::group::${GROUP_PREFIX} ${CMD}"
  CMD_EXIT=0
  set +e
  bash "${GITHUB_ACTION_PATH}/scripts/core/run-with-liveness-timeout.sh" \
    --log-file "${HOMEBOY_OUTPUT_DIR}/${OUTPUT_STEM}.log" \
    "homeboy ${CMD}" bash -c "${FULL_CMD}"
  CMD_EXIT=$?
  cat "${HOMEBOY_OUTPUT_DIR}/${OUTPUT_STEM}.log"
  set -e
  echo "::endgroup::"

  STRUCTURED_OUTPUT=true
  if [ ! -s "${OUTPUT_JSON}" ] || ! jq -e 'type == "object"' "${OUTPUT_JSON}" >/dev/null 2>&1; then
    STRUCTURED_OUTPUT=false
    echo "::error::homeboy ${CMD} did not write valid structured output to ${OUTPUT_JSON}"
  elif [ "$(printf '%s' "${CMD}" | awk '{print $1}')" = "bench" ]; then
    cp "${OUTPUT_JSON}" "${HOMEBOY_OUTPUT_DIR}/${OUTPUT_STEM}.json"
  fi

  if [ "${CMD_EXIT}" -eq 0 ] && [ "${STRUCTURED_OUTPUT}" = true ]; then
    echo "::notice::homeboy ${CMD}: PASSED"
    RESULTS=$(echo "${RESULTS}" | jq -c --arg cmd "${CMD}" '. + {($cmd): "pass"}')
  elif [ "${CMD_EXIT}" -eq 0 ]; then
    echo "::error::homeboy ${CMD}: FAILED because required structured output was missing or malformed."
    RESULTS=$(echo "${RESULTS}" | jq -c --arg cmd "${CMD}" '. + {($cmd): "fail"}')
    OVERALL_EXIT=1
  elif [ "${CMD_EXIT}" -eq 124 ]; then
    echo "::error::homeboy ${CMD}: TIMED OUT (exit code 124); inspect the retained command log above."
    RESULTS=$(echo "${RESULTS}" | jq -c --arg cmd "${CMD}" '. + {($cmd): "timeout"}')
    OVERALL_EXIT=1
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
