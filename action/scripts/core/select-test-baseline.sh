#!/usr/bin/env bash

set -euo pipefail

# shellcheck disable=SC1091
source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

command="${TEST_SHARD_COMMAND:?TEST_SHARD_COMMAND is required}"
results_file="${TEST_SHARD_RESULTS_FILE:?TEST_SHARD_RESULTS_FILE is required}"
result_file="${TEST_SHARD_RESULT_FILE:?TEST_SHARD_RESULT_FILE is required}"
result_filename="$(command_result_filename "${command}")"

[ -s "${results_file}" ] || { echo "::error::Candidate shard results are missing: ${results_file}" >&2; exit 1; }
[ -s "${result_file}" ] || { echo "::error::Candidate shard result is missing: ${result_file}" >&2; exit 1; }

status="$(jq -r --arg command "${command}" '.[$command] // empty' "${results_file}")"
case "${status}" in
  pass|fail|timeout) ;;
  *) echo "::error::Candidate shard results do not declare a valid ${command} status." >&2; exit 1 ;;
esac

jq -e --arg root "${command%% *}" '
  .schema == "homeboy/command-result/v3" and .command == $root
  and (.success | type == "boolean") and (.status | type == "string")
  and (.exit_code | type == "number" and floor == .)
  and (.data.test_counts | type == "object")
' "${result_file}" >/dev/null || {
  echo "::error::Candidate shard result is not a valid ${result_filename} command result." >&2
  exit 1
}

if [ "${status}" = pass ]; then
  echo 'baseline-command=' >> "${GITHUB_OUTPUT}"
else
  echo "baseline-command=${command}" >> "${GITHUB_OUTPUT}"
fi
