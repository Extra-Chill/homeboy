#!/usr/bin/env bash

# Retry apparent PR-introduced test failures before the differential gate
# blames the PR for them. Extra-Chill/homeboy#14984.
#
# Homeboy has no CLI flag to re-run a specific set of test identities -- only
# a generic `homeboy review test ... -- ARGS` passthrough whose mapping to
# the underlying runner is owned by the extension and not guaranteed to
# filter by exact test name. This script therefore drives the underlying
# Rust test runner directly -- `cargo nextest` or plain `cargo test` -- which
# is the "whatever runner the phase already uses for that component's
# language" fallback the tracking issue authorizes when Homeboy itself lacks
# the capability. Any other runner (or a nextest/cargo binary that is not on
# PATH) is unsupported: this writes a sidecar that changes nothing, and the
# gate keeps its current, safe behavior of blaming the PR.
#
# Each candidate-only failed test identity is retried alone -- not as part of
# a batched, filtered rerun -- so a flaky test cannot hide behind, or be
# blamed for, contention with another test in the same process. A test that
# passes even once within its retry budget is flaky; a test that fails every
# attempt stays introduced.
#
# Reads (env, required):
#   COMMAND      -- the quality command, e.g. "review test" or "test"
#   CURRENT_DIR  -- candidate homeboy-ci-results dir (has {stem}.test-outcomes.json)
#   BASE_DIR     -- baseline homeboy-ci-results dir
#   WORKSPACE    -- a checked-out, buildable candidate source tree
#
# Reads (env, optional):
#   MAX_RETRIES  -- retry attempts per test identity (default 2)
#
# Writes:
#   ${CURRENT_DIR}/${stem}.test-retry.json -- a homeboy/test-retry/v1 sidecar.
#   apply-differential-gate.py reads it to exclude confirmed-flaky identities
#   from introduced-failure attribution. Absence (this script never having
#   run, or having nothing to retry) is read as "no retry was attempted" --
#   never as "retried and still failing".
#
# This script never exits non-zero on its own account: retrying is advisory,
# and the pass/fail decision belongs to apply-differential-gate.py reading
# the sidecar it writes. A failure to retry is not a failure of the gate.

set -euo pipefail

action_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck disable=SC1091
source "${action_root}/scripts/core/lib.sh"

command="${COMMAND:?COMMAND is required}"
current_dir="${CURRENT_DIR:?CURRENT_DIR is required}"
base_dir="${BASE_DIR:?BASE_DIR is required}"
workspace="${WORKSPACE:?WORKSPACE is required}"
max_retries="${MAX_RETRIES:-2}"

if ! [[ "${max_retries}" =~ ^[0-9]+$ ]]; then
  echo "::error::Differential gate retry: MAX_RETRIES must be a non-negative integer, got '${max_retries}'." >&2
  exit 1
fi

# Mirrors apply-differential-gate.py's test_evidence_command_identity(): the
# sidecar's `command` field must match exactly what the gate will look up.
test_evidence_command_identity() {
  local cmd="$1"
  # shellcheck disable=SC2206
  local parts=(${cmd})
  if [ "${#parts[@]}" -ge 2 ] && [ "${parts[0]}" = "review" ] && [ "${parts[1]}" = "test" ]; then
    printf 'review test\n'
  elif [ "${#parts[@]}" -ge 1 ] && [ "${parts[0]}" = "test" ]; then
    printf 'test\n'
  else
    printf '%s\n' "$(printf '%s' "${cmd}" | xargs)"
  fi
}

stem="$(command_output_stem "${command}")"
output_file="${current_dir}/${stem}.test-retry.json"
evidence_command="$(test_evidence_command_identity "${command}")"

write_sidecar() {
  local attempted_json="$1" flaky_json="$2" still_failing_json="$3" runner="$4" retries_run="$5"
  local extra="$6"
  jq -cn \
    --arg command "${evidence_command}" \
    --arg runner "${runner}" \
    --argjson attempted "${attempted_json}" \
    --argjson flaky "${flaky_json}" \
    --argjson still_failing "${still_failing_json}" \
    --argjson max_retries "${retries_run}" \
    --argjson extra "${extra:-null}" \
    '{schema:"homeboy/test-retry/v1",command:$command,runner:$runner,attempted:$attempted,flaky:$flaky,still_failing:$still_failing,max_retries:$max_retries} + (if $extra == null then {} else $extra end)' \
    > "${output_file}"
}

json_array_of() {
  # Prints "[]" for an empty argument list; jq -R/-s otherwise assembles the
  # array from one string per line.
  if [ "$#" -eq 0 ]; then
    printf '[]\n'
    return
  fi
  printf '%s\n' "$@" | jq -R . | jq -sc .
}

introduced_json="$(python3 "${action_root}/scripts/core/apply-differential-gate.py" --emit-introduced "${command}" "${current_dir}" "${base_dir}")"
runner="$(printf '%s' "${introduced_json}" | jq -r '.runner // ""')"
mapfile -t introduced_ids < <(printf '%s' "${introduced_json}" | jq -r '.introduced[]')

if [ "${#introduced_ids[@]}" -eq 0 ]; then
  echo "Differential gate retry: no candidate-only failed test identities for ${command}; nothing to retry."
  exit 0
fi

runner_lc="$(printf '%s' "${runner}" | tr '[:upper:]' '[:lower:]')"
mode=""
case "${runner_lc}" in
  *nextest*)
    command -v cargo-nextest >/dev/null 2>&1 && mode="nextest"
    ;;
  *cargo*)
    command -v cargo >/dev/null 2>&1 && mode="cargo-test"
    ;;
esac

attempted_json="$(json_array_of "${introduced_ids[@]}")"

if [ -z "${mode}" ]; then
  write_sidecar "${attempted_json}" '[]' "${attempted_json}" "${runner}" 0 "$(jq -cn --arg runner "${runner}" '{unsupported_runner:$runner}')"
  echo "::notice::Differential gate retry: no supported retry runner for ${command} (runner='${runner}'); ${#introduced_ids[@]} candidate-only test identity(s) were not retried and remain introduced. Extra-Chill/homeboy#14984 tracks retry support for runners beyond cargo test/cargo nextest." >&2
  exit 0
fi

run_single_test() {
  local id="$1"
  local log
  log="$(mktemp)"
  local ok=1
  case "${mode}" in
    nextest)
      if (cd "${workspace}" && cargo nextest run -E "test(=${id})") >"${log}" 2>&1; then
        grep -Eq '1 tests? run: 1 passed' "${log}" && ok=0
      fi
      ;;
    cargo-test)
      # `--tests` restricts to lib/integration test binaries, excluding
      # doctests: a Homeboy test identity is never a doctest path, and
      # letting doctests run here would fail the retry for reasons that
      # have nothing to do with the identity being retried.
      if (cd "${workspace}" && cargo test --tests -- --exact "${id}") >"${log}" 2>&1; then
        grep -Eq '(^| )1 passed(;| )' "${log}" && ok=0
      fi
      ;;
  esac
  rm -f "${log}"
  return "${ok}"
}

remaining=("${introduced_ids[@]}")
flaky=()

for ((attempt = 1; attempt <= max_retries; attempt++)); do
  [ "${#remaining[@]}" -eq 0 ] && break
  next_remaining=()
  for id in "${remaining[@]}"; do
    if run_single_test "${id}"; then
      flaky+=("${id}")
    else
      next_remaining+=("${id}")
    fi
  done
  remaining=("${next_remaining[@]}")
done

still_failing=("${remaining[@]}")

flaky_json="$(json_array_of "${flaky[@]}")"
still_failing_json="$(json_array_of "${still_failing[@]}")"

write_sidecar "${attempted_json}" "${flaky_json}" "${still_failing_json}" "${runner}" "${max_retries}" 'null'

if [ "${#flaky[@]}" -gt 0 ]; then
  flaky_list="$(IFS=', '; echo "${flaky[*]}")"
  echo "::warning::Differential gate retry: ${#flaky[@]} candidate-only test identity(s) for ${command} passed in isolation within ${max_retries} retry attempt(s) and were classified flaky: ${flaky_list}." >&2
fi
if [ "${#still_failing[@]}" -gt 0 ]; then
  still_failing_list="$(IFS=', '; echo "${still_failing[*]}")"
  echo "::warning::Differential gate retry: ${#still_failing[@]} candidate-only test identity(s) for ${command} failed every retry attempt in isolation and remain introduced: ${still_failing_list}." >&2
fi

exit 0
