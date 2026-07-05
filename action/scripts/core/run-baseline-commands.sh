#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

EFFECTIVE_COMMANDS="${COMMANDS:-review audit,review lint,review test}"
BASELINE_COMMANDS_INPUT="${BASELINE_COMMANDS:-auto}"

if [ "${HOMEBOY_DIFFERENTIAL_GATING:-false}" != "true" ]; then
  echo "Differential gating disabled; skipping baseline run"
  exit 0
fi

if [ "${SCOPE_CONTEXT:-}" != "pr" ] || [ -z "${SCOPE_BASE_REF:-}" ]; then
  echo "Differential gating requires PR scope with a resolved base ref; skipping baseline run"
  exit 0
fi

TRACKED_DIRTY="$(git status --porcelain --untracked-files=no)"
if [ -n "${TRACKED_DIRTY}" ]; then
  echo "::warning::Tracked changes exist before the baseline checkout; skipping differential baseline run"
  exit 0
fi

BASELINE_RUN_COMMANDS=()
ORDERED_COMMANDS="$(resolve_baseline_commands "${EFFECTIVE_COMMANDS}" "${BASELINE_COMMANDS_INPUT}")"
IFS=',' read -ra CMD_ARRAY <<< "${ORDERED_COMMANDS}"
for CMD in "${CMD_ARRAY[@]}"; do
  CMD="$(echo "${CMD}" | xargs)"
  case "$(quality_base_command "${CMD}")" in
    audit|lint|test)
      BASELINE_RUN_COMMANDS+=("${CMD}")
      ;;
  esac
done

if [ "${#BASELINE_RUN_COMMANDS[@]}" -eq 0 ]; then
  echo "No audit/lint/test commands requested; skipping differential baseline run"
  exit 0
fi

ORIGINAL_REF="$(git symbolic-ref --quiet --short HEAD 2>/dev/null || git rev-parse HEAD)"
BASE_OUTPUT_DIR="$(mktemp -d)"
echo "HOMEBOY_BASE_OUTPUT_DIR=${BASE_OUTPUT_DIR}" >> "${GITHUB_ENV}"
BASELINE_STATUS_JSON="${BASE_OUTPUT_DIR}/baseline-status.json"
printf '{}\n' > "${BASELINE_STATUS_JSON}"

restore_original_ref() {
  git checkout -q "${ORIGINAL_REF}" || true
}
trap restore_original_ref EXIT

echo "Checking out baseline ref ${SCOPE_BASE_REF} for differential gating"
git checkout -q --detach "${SCOPE_BASE_REF}"

COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"
GROUP_PREFIX="${RUN_GROUP_PREFIX:-homeboy baseline}"

for CMD in "${BASELINE_RUN_COMMANDS[@]}"; do
  OUTPUT_STEM="$(command_output_stem "${CMD}")"
  OUTPUT_JSON="${BASE_OUTPUT_DIR}/${OUTPUT_STEM}.json"
  FULL_CMD="$(build_run_command "${CMD}" "${COMP_ID}" "${WORKSPACE}" "${OUTPUT_JSON}")"

  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Running baseline: ${FULL_CMD}"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  echo "::group::${GROUP_PREFIX} ${CMD}"
  set +e
  eval "${FULL_CMD}" 2>&1 | tee "${BASE_OUTPUT_DIR}/${OUTPUT_STEM}.log"
  CMD_EXIT=${PIPESTATUS[0]}
  set -e
  echo "::endgroup::"

  if [ ! -s "${OUTPUT_JSON}" ]; then
    echo "::warning::baseline homeboy ${CMD} did not write structured output to ${OUTPUT_JSON}"
  fi

  STRUCTURED_OUTPUT=false
  if [ -s "${OUTPUT_JSON}" ]; then
    STRUCTURED_OUTPUT=true
  fi

  STATUS="pass"
  if [ "${CMD_EXIT}" -ne 0 ]; then
    STATUS="fail"
  fi

  tmp_status="$(mktemp)"
  jq -c \
    --arg cmd "${CMD}" \
    --arg status "${STATUS}" \
    --arg command "${FULL_CMD}" \
    --arg output_json "${OUTPUT_JSON}" \
    --arg log_path "${BASE_OUTPUT_DIR}/${OUTPUT_STEM}.log" \
    --argjson exit_code "${CMD_EXIT}" \
    --argjson structured_output "${STRUCTURED_OUTPUT}" \
    '. + {($cmd): {status: $status, exit_code: $exit_code, command: $command, output_json: $output_json, log_path: $log_path, structured_output: $structured_output}}' \
    "${BASELINE_STATUS_JSON}" > "${tmp_status}"
  mv "${tmp_status}" "${BASELINE_STATUS_JSON}"

  echo "Baseline homeboy ${CMD} exited ${CMD_EXIT}"
done

echo "Baseline outputs captured in ${BASE_OUTPUT_DIR}"
