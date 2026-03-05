#!/usr/bin/env bash

set -euo pipefail

OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
RESULTS_JSON="${RESULTS:-{}}"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"

if [ -z "${OUTPUT_DIR}" ] || [ ! -d "${OUTPUT_DIR}" ]; then
  echo "No output directory available; skipping failure digest"
  exit 0
fi

DIGEST_FILE=$(python3 "${GITHUB_ACTION_PATH}/scripts/build-failure-digest.py" "${OUTPUT_DIR}" "${RESULTS_JSON}" "${RUN_URL}" 2>/dev/null || true)
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
