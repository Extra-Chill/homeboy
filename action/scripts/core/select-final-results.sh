#!/usr/bin/env bash

set -euo pipefail

RESULTS="${FIRST_RESULTS:-}"
COMMANDS="${COMMANDS:-}"

results_are_complete() {
  [ -n "${RESULTS}" ] || return 1
  printf '%s\n' "${RESULTS}" | jq -e --arg commands "${COMMANDS}" '
    . as $results |
    ($commands | split(",") | map(gsub("^\\s+|\\s+$"; "")) | map(select(length > 0))) as $expected |
    ($results | type) == "object"
    and all($expected[]; . as $command | $results[$command] == "pass" or $results[$command] == "fail")
  ' >/dev/null 2>&1
}

if ! results_are_complete; then
  if [ -n "${COMMANDS}" ]; then
    RESULTS="$(printf '%s\n' "${COMMANDS}" | jq -Rc '
      split(",") | map(gsub("^\\s+|\\s+$"; "") | select(length > 0)) | map({key: ., value: "fail"}) | from_entries
    ')"
    echo "::error::Current Homeboy command results were missing, malformed, interrupted, or incomplete; marking all expected commands failed."
  else
    RESULTS='{}'
  fi
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
