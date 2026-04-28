#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SELECT_RESULTS="${SCRIPT_DIR}/select-final-results.sh"

assert_equals() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual:   %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

read_multiline_output() {
  local file="$1"
  local key="$2"
  local marker value line in_value=false

  while IFS= read -r line; do
    if [ "${in_value}" = true ]; then
      if [ "${line}" = "${marker}" ]; then
        printf '%s' "${value%$'\n'}"
        return 0
      fi
      value+="${line}"$'\n'
      continue
    fi

    case "${line}" in
      "${key}<<"*)
        marker="${line#*<<}"
        value=""
        in_value=true
        ;;
    esac
  done < "${file}"

  return 1
}

run_select() {
  local first_results="$1"
  local output_file

  output_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-select-results.XXXXXX")"
  GITHUB_OUTPUT="${output_file}" \
    FIRST_RESULTS="${first_results}" \
    HOMEBOY_DIFFERENTIAL_GATING=false \
    bash "${SELECT_RESULTS}"

  printf '%s\n' "${output_file}"
}

pass_output="$(run_select '{"lint":"pass"}')"
assert_equals '{"lint":"pass"}' "$(read_multiline_output "${pass_output}" results)" "passing JSON result round-trips without extra braces"

empty_output="$(run_select '')"
assert_equals '{}' "$(read_multiline_output "${empty_output}" results)" "empty result falls back to empty object"

printf 'All select-final-results checks passed.\n'
