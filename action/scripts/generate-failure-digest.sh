#!/usr/bin/env bash

set -euo pipefail

OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
RESULTS_JSON="${RESULTS:-{}}"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
HOMEBOY_CLI_VERSION="${HOMEBOY_CLI_VERSION:-unknown}"
HOMEBOY_EXTENSION_ID="${HOMEBOY_EXTENSION_ID:-auto}"
HOMEBOY_EXTENSION_SOURCE="${HOMEBOY_EXTENSION_SOURCE:-auto}"
HOMEBOY_EXTENSION_REVISION="${HOMEBOY_EXTENSION_REVISION:-unknown}"
HOMEBOY_ACTION_REF="${HOMEBOY_ACTION_REF:-unknown}"
HOMEBOY_ACTION_REPOSITORY="${HOMEBOY_ACTION_REPOSITORY:-unknown}"

if [ -z "${OUTPUT_DIR}" ] || [ ! -d "${OUTPUT_DIR}" ]; then
  echo "No output directory available; skipping failure digest"
  exit 0
fi

DIGEST_FILE=$(python3 "${GITHUB_ACTION_PATH}/scripts/build-failure-digest.py" \
  "${OUTPUT_DIR}" \
  "${RESULTS_JSON}" \
  "${RUN_URL}" \
  "${HOMEBOY_CLI_VERSION}" \
  "${HOMEBOY_EXTENSION_ID}" \
  "${HOMEBOY_EXTENSION_SOURCE}" \
  "${HOMEBOY_EXTENSION_REVISION}" \
  "${HOMEBOY_ACTION_REPOSITORY}" \
  "${HOMEBOY_ACTION_REF}" \
  2>/dev/null || true)
if [ -z "${DIGEST_FILE}" ] || [ ! -f "${DIGEST_FILE}" ]; then
  echo "Failure digest generation returned no file"
  exit 0
fi

echo "HOMEBOY_FAILURE_DIGEST_FILE=${DIGEST_FILE}" >> "${GITHUB_ENV}"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo ""
    cat "${DIGEST_FILE}"
  } >> "${GITHUB_STEP_SUMMARY}"
fi

echo "Failure digest generated at ${DIGEST_FILE}"
