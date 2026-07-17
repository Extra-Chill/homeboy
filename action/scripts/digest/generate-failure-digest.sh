#!/usr/bin/env bash

set -euo pipefail

OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
RESULTS_JSON="${RESULTS:-"{}"}"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
HOMEBOY_CLI_VERSION="${HOMEBOY_CLI_VERSION:-unknown}"
HOMEBOY_EXTENSION_ID="${HOMEBOY_EXTENSION_ID:-auto}"
HOMEBOY_EXTENSION_SOURCE="${HOMEBOY_EXTENSION_SOURCE:-auto}"
HOMEBOY_EXTENSION_REVISION="${HOMEBOY_EXTENSION_REVISION:-unknown}"
HOMEBOY_ACTION_REF="${HOMEBOY_ACTION_REF:-unknown}"
HOMEBOY_ACTION_REPOSITORY="${HOMEBOY_ACTION_REPOSITORY:-unknown}"
COMMANDS_CSV="${COMMANDS:-}"
COMPONENT_NAME="${COMPONENT_NAME:-${HOMEBOY_COMPONENT_ID:-}}"

append_local_reproduction_commands() {
  local digest_file="$1"
  local failed_commands
  local command
  local scope_args=""

  failed_commands="$(jq -r '
    to_entries[]
    | select(.value == "fail" and (.key == "review lint" or .key == "review test"))
    | .key
  ' <<< "${RESULTS_JSON}" 2>/dev/null || true)"

  [ -n "${failed_commands}" ] || return 0

  if [ -z "${COMPONENT_NAME}" ]; then
    return 0
  fi

  if [ "${SCOPE_MODE:-full}" = "changed" ] && [ -n "${SCOPE_BASE_REF:-}" ]; then
    scope_args=" --changed-since ${SCOPE_BASE_REF}"
  fi

  {
    printf '\n### Local reproduction\n'
    printf -- '- Scope context: `%s`\n' "${SCOPE_CONTEXT:-unknown}"
    printf -- '- Scope mode: `%s`\n' "${SCOPE_MODE:-full}"
    printf -- '- Scope base ref: `%s`\n' "${SCOPE_BASE_REF:-none}"
    printf '\n```bash\n'
    while IFS= read -r command; do
      [ -n "${command}" ] || continue
      case "${command}" in
        "review "*) printf 'homeboy %s %s --path .%s\n' "${command}" "${COMPONENT_NAME}" "${scope_args}" ;;
        lint|test) printf 'homeboy review %s %s --path .%s\n' "${command}" "${COMPONENT_NAME}" "${scope_args}" ;;
      esac
    done <<< "${failed_commands}"
    printf '```\n'
  } >> "${digest_file}"
}

command_artifact_stem() {
  printf '%s' "$1" | sed -E 's/[^[:alnum:]._-]+/-/g; s/^-+//; s/-+$//'
}

append_differential_evidence() {
  local digest_file="$1"
  local baseline_status_file="${OUTPUT_DIR}/baseline-status.json"
  local rows

  rows="$(jq -r '
    to_entries[]
    | select(.value == "baseline_red" or .value == "inconclusive")
    | [.key, .value] | @tsv
  ' <<< "${RESULTS_JSON}" 2>/dev/null || true)"

  [ -n "${rows}" ] || return 0

  {
    printf '\n### Differential baseline evidence\n'
    printf -- '- Classification: baseline failures or missing comparable counts prevented candidate regression enforcement.\n'
    if [ -f "${baseline_status_file}" ]; then
      printf -- '- Baseline status artifact: `baseline-status.json`\n'
    fi
  } >> "${digest_file}"

  while IFS=$'\t' read -r command status; do
    [ -n "${command}" ] || continue
    local baseline_command baseline_exit baseline_result candidate_result artifact_stem baseline_json baseline_log
    baseline_command="$(jq -r --arg cmd "${command}" '.[$cmd].command // ("homeboy " + $cmd)' "${baseline_status_file}" 2>/dev/null || printf 'homeboy %s' "${command}")"
    baseline_exit="$(jq -r --arg cmd "${command}" '.[$cmd].exit_code // "unknown"' "${baseline_status_file}" 2>/dev/null || printf 'unknown')"
    baseline_result="$(jq -r --arg cmd "${command}" '.[$cmd].status // "unknown"' "${baseline_status_file}" 2>/dev/null || printf 'unknown')"
    candidate_result="$(jq -r --arg cmd "${command}" '.[$cmd] // "unknown"' <<< "${RESULTS_JSON}" 2>/dev/null || printf 'unknown')"
    artifact_stem="$(command_artifact_stem "${command}")"
    baseline_json="baseline-${artifact_stem}.json"
    baseline_log="baseline-${artifact_stem}.log"

    {
      printf -- '- `%s`: **%s**\n' "${command}" "${status}"
      printf '  - Baseline command: `%s`\n' "${baseline_command}"
      printf '  - Baseline result: `%s` (exit `%s`)\n' "${baseline_result}" "${baseline_exit}"
      printf '  - Candidate result: `%s`\n' "${candidate_result}"
      printf '  - Artifact refs: `%s`, `%s`, `%s`\n' "${artifact_stem}.json" "${baseline_json}" "${baseline_log}"
    } >> "${digest_file}"
  done <<< "${rows}"
}

if [ -z "${OUTPUT_DIR}" ] || [ ! -d "${OUTPUT_DIR}" ]; then
  echo "No output directory available; skipping failure digest"
  exit 0
fi

TOOLING_JSON="${OUTPUT_DIR}/failure-digest-tooling.json"
DIGEST_FILE="${OUTPUT_DIR}/failure-digest.md"

jq -n \
  --arg homeboy_cli_version "${HOMEBOY_CLI_VERSION}" \
  --arg extension_id "${HOMEBOY_EXTENSION_ID}" \
  --arg extension_source "${HOMEBOY_EXTENSION_SOURCE}" \
  --arg extension_revision "${HOMEBOY_EXTENSION_REVISION}" \
  --arg action_repository "${HOMEBOY_ACTION_REPOSITORY}" \
  --arg action_ref "${HOMEBOY_ACTION_REF}" \
  '{
    homeboy_cli_version: $homeboy_cli_version,
    extension_id: $extension_id,
    extension_source: $extension_source,
    extension_revision: $extension_revision,
    action_repository: $action_repository,
    action_ref: $action_ref
  }' > "${TOOLING_JSON}"

ARGS=(
  report failure-digest
  --output-dir "${OUTPUT_DIR}"
  --results "${RESULTS_JSON}"
  --run-url "${RUN_URL}"
  --tooling-json "${TOOLING_JSON}"
  --commands "${COMMANDS_CSV}"
)

if ! homeboy "${ARGS[@]}" > "${DIGEST_FILE}" 2>/dev/null; then
  rm -f "${DIGEST_FILE}"
fi

if [ ! -f "${DIGEST_FILE}" ]; then
  echo "Failure digest generation returned no file"
  exit 0
fi

if [ ! -s "${DIGEST_FILE}" ]; then
  echo "Failure digest generation returned no file"
  rm -f "${DIGEST_FILE}"
  exit 0
fi

append_local_reproduction_commands "${DIGEST_FILE}"
append_differential_evidence "${DIGEST_FILE}"

if [ -n "${GITHUB_ENV:-}" ]; then
  echo "HOMEBOY_FAILURE_DIGEST_FILE=${DIGEST_FILE}" >> "${GITHUB_ENV}"
fi

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo ""
    cat "${DIGEST_FILE}"
  } >> "${GITHUB_STEP_SUMMARY}"
fi

echo "Failure digest generated at ${DIGEST_FILE}"
