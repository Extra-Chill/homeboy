#!/usr/bin/env bash

# Reconcile independently produced candidate and baseline evidence. Artifacts
# are untrusted transport: every identity field is checked before a verdict.
set -euo pipefail

artifact_root="${PHASE_ARTIFACT_ROOT:?PHASE_ARTIFACT_ROOT is required}"
command="${COMMAND:?COMMAND is required}"
action_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fail_closed() {
  echo "::error::$1"
  echo "results={\"${command}\":\"fail\"}" >> "${GITHUB_OUTPUT}"
  exit 1
}

manifest_for() {
  local phase="$1"
  local path attempt selected="" selected_attempt=0 selected_count=0
  while IFS= read -r path; do
    attempt="$(jq -r '.run_attempt // 0' "${path}" 2>/dev/null || printf '0')"
    [[ "${attempt}" =~ ^[0-9]+$ ]] || attempt=0
    if [ "${attempt}" -gt "${selected_attempt}" ]; then
      selected="${path}"
      selected_attempt="${attempt}"
      selected_count=1
    elif [ "${attempt}" -eq "${selected_attempt}" ] && [ "${attempt}" -gt 0 ]; then
      selected_count=$((selected_count + 1))
    fi
  done < <(find "${artifact_root}" -path "*/${phase}/manifest.json" -type f | sort)
  [ -n "${selected}" ] && [ "${selected_count}" -eq 1 ] || fail_closed "Expected one newest ${phase} provenance artifact; found ${selected_count} at attempt ${selected_attempt}."
  printf '%s\n' "${selected}"
}

candidate_manifest="$(manifest_for candidate)"
candidate_dir="$(dirname "${candidate_manifest}")"

validate_manifest() {
  local phase="$1" manifest="$2" expected_sha="$3"
  jq -e \
    --arg phase "${phase}" --arg repository "${REPOSITORY:?REPOSITORY is required}" \
    --arg candidate_sha "${CANDIDATE_SHA:?CANDIDATE_SHA is required}" --arg base_sha "${BASE_SHA:?BASE_SHA is required}" \
    --arg command "${command}" --arg action_revision "${ACTION_REVISION:?ACTION_REVISION is required}" \
    --arg expected_sha "${expected_sha}" '
      type == "object"
      and .phase == $phase
      and .repository == $repository
      and .candidate_sha == $candidate_sha
      and .base_sha == $base_sha
      and .checkout_sha == $expected_sha
      and .command == $command
      and .action_revision == $action_revision
      and (.run_attempt | type == "number" and . > 0 and . <= (env.RUN_ATTEMPT | tonumber))
      and (.component | type == "string" and length > 0)
      and (.cli_revision | type == "string" and length > 0)
      and (.binary_sha256 | type == "string" and test("^[0-9a-f]{64}$"))
      and (.results | type == "object" and (.[$command] == "pass" or .[$command] == "fail" or .[$command] == "timeout"))
    ' "${manifest}" >/dev/null 2>&1 || fail_closed "${phase} phase provenance is missing, malformed, or does not match this workflow."
}

validate_manifest candidate "${candidate_manifest}" "${CANDIDATE_SHA}"
candidate_results="$(jq -c '.results' "${candidate_manifest}")"
candidate_output="${candidate_dir}/homeboy-ci-results"
[ -d "${candidate_output}" ] || fail_closed "Candidate result payload is missing."

if [ "${REQUIRE_BASELINE:-false}" = "true" ]; then
  baseline_manifest="$(manifest_for baseline)"
  baseline_dir="$(dirname "${baseline_manifest}")"
  validate_manifest baseline "${baseline_manifest}" "${BASE_SHA}"
  candidate_component="$(jq -r '.component' "${candidate_manifest}")"
  baseline_component="$(jq -r '.component' "${baseline_manifest}")"
  candidate_cli="$(jq -r '.cli_revision' "${candidate_manifest}")"
  baseline_cli="$(jq -r '.cli_revision' "${baseline_manifest}")"
  candidate_binary="$(jq -r '.binary_sha256' "${candidate_manifest}")"
  baseline_binary="$(jq -r '.binary_sha256' "${baseline_manifest}")"
  [ "${candidate_component}" = "${baseline_component}" ] || fail_closed "Candidate and baseline component provenance differs."
  [ "${candidate_cli}" = "${baseline_cli}" ] || fail_closed "Candidate and baseline CLI provenance differs."
  [ "${candidate_binary}" = "${baseline_binary}" ] || fail_closed "Candidate and baseline binary provenance differs."
  baseline_results="$(jq -c '.results' "${baseline_manifest}")"
  baseline_output="${baseline_dir}/homeboy-ci-results"
  [ -d "${baseline_output}" ] || fail_closed "Baseline result payload is missing."

  for baseline_file in "${baseline_output}"/*.json; do
    [ -f "${baseline_file}" ] || continue
    name="$(basename "${baseline_file}")"
    if [ "${name}" = "baseline-status.json" ]; then
      cp "${baseline_file}" "${candidate_output}/${name}"
    else
      cp "${baseline_file}" "${candidate_output}/baseline-${name}"
    fi
  done

# apply-differential-gate expects baseline command metadata beside its payload.
baseline_status="${baseline_output}/baseline-status.json"
if [ ! -f "${baseline_status}" ]; then
  baseline_status_value="$(jq -r --arg command "${command}" '.[$command]' <<< "${baseline_results}")"
  structured=false
  stem="$(printf '%s' "${command}" | tr -c 'A-Za-z0-9._-' '-')"
  [ -f "${baseline_output}/${stem}.json" ] && structured=true
  jq -cn --arg command "${command}" --arg status "${baseline_status_value}" --argjson structured "${structured}" \
    '{($command): {status:$status, exit_code:(if $status == "pass" then 0 elif $status == "timeout" then 124 else 1 end), command:$command, structured_output:$structured}}' > "${baseline_status}"
fi
fi

if [ "${REQUIRE_BASELINE:-false}" = "true" ]; then
  results="$(python3 "${action_root}/scripts/core/apply-differential-gate.py" "${candidate_results}" "${candidate_output}" "${baseline_output}")"
else
  results="${candidate_results}"
fi

{
  echo 'results<<__HOMEBOY_RESULTS__'
  printf '%s\n' "${results}"
  echo '__HOMEBOY_RESULTS__'
} >> "${GITHUB_OUTPUT}"

component="$(jq -r '.component' "${candidate_manifest}")"
printf 'component=%s\noutput-dir=%s\n' "${component}" "${candidate_output}" >> "${GITHUB_OUTPUT}"

RESULTS="${results}" COMMANDS="${command}" OPERATIONS_RESULTS='' PR_ACTIVE='' \
  bash "${action_root}/scripts/core/enforce-final-status.sh"
