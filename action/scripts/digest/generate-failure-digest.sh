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
AUTOFIX_ENABLED="${AUTOFIX_ENABLED:-false}"
AUTOFIX_ATTEMPTED="${AUTOFIX_ATTEMPTED:-false}"
AUTOFIX_COMMANDS="${AUTOFIX_COMMANDS:-}"

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
  --autofix-commands "${AUTOFIX_COMMANDS}"
)

if [ "${AUTOFIX_ENABLED}" = "true" ]; then
  ARGS+=(--autofix-enabled)
fi

if [ "${AUTOFIX_ATTEMPTED}" = "true" ]; then
  ARGS+=(--autofix-attempted)
fi

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
