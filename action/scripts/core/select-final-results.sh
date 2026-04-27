#!/usr/bin/env bash

set -euo pipefail

RESULTS="${FIRST_RESULTS:-{}}"

if [ "${HOMEBOY_DIFFERENTIAL_GATING:-false}" = "true" ] \
  && [ -n "${HOMEBOY_OUTPUT_DIR:-}" ] \
  && [ -d "${HOMEBOY_OUTPUT_DIR:-}" ] \
  && [ -n "${HOMEBOY_BASE_OUTPUT_DIR:-}" ] \
  && [ -d "${HOMEBOY_BASE_OUTPUT_DIR:-}" ]; then
  RESULTS="$(python3 "${GITHUB_ACTION_PATH}/scripts/core/apply-differential-gate.py" \
    "${RESULTS}" \
    "${HOMEBOY_OUTPUT_DIR}" \
    "${HOMEBOY_BASE_OUTPUT_DIR}")"
fi

echo "results=${RESULTS}" >> "${GITHUB_OUTPUT}"
