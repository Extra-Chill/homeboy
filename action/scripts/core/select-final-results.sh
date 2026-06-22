#!/usr/bin/env bash

set -euo pipefail

RESULTS="${FIRST_RESULTS:-}"
if [ -z "${RESULTS}" ]; then
  RESULTS='{}'
fi

if [ "${HOMEBOY_DIFFERENTIAL_GATING:-false}" = "true" ] \
  && [ -n "${HOMEBOY_OUTPUT_DIR:-}" ] \
  && [ -d "${HOMEBOY_OUTPUT_DIR:-}" ] \
  && [ -n "${HOMEBOY_BASE_OUTPUT_DIR:-}" ] \
  && [ -d "${HOMEBOY_BASE_OUTPUT_DIR:-}" ]; then
  for baseline_file in "${HOMEBOY_BASE_OUTPUT_DIR}"/*.json; do
    [ -f "${baseline_file}" ] || continue
    name="$(basename "${baseline_file}")"
    if [ "${name}" = "baseline-status.json" ]; then
      cp "${baseline_file}" "${HOMEBOY_OUTPUT_DIR}/${name}"
    else
      cp "${baseline_file}" "${HOMEBOY_OUTPUT_DIR}/baseline-${name}"
    fi
  done
  for baseline_log in "${HOMEBOY_BASE_OUTPUT_DIR}"/*.log; do
    [ -f "${baseline_log}" ] || continue
    cp "${baseline_log}" "${HOMEBOY_OUTPUT_DIR}/baseline-$(basename "${baseline_log}")"
  done

  RESULTS="$(python3 "${GITHUB_ACTION_PATH}/scripts/core/apply-differential-gate.py" \
    "${RESULTS}" \
    "${HOMEBOY_OUTPUT_DIR}" \
    "${HOMEBOY_BASE_OUTPUT_DIR}")"
fi

{
  echo 'results<<__HOMEBOY_RESULTS__'
  printf '%s\n' "${RESULTS}"
  echo '__HOMEBOY_RESULTS__'
} >> "${GITHUB_OUTPUT}"
