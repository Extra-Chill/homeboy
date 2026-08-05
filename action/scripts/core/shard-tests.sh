#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "${SCRIPT_DIR}/lib.sh"

fail() { echo "::error::$1" >&2; exit 1; }
digest() { printf '%s' "$1" | shasum -a 256 | awk '{print $1}'; }

plan() {
  local inventory="${TEST_INVENTORY_FILE:?TEST_INVENTORY_FILE is required}"
  local output="${TEST_SHARD_PLAN_FILE:?TEST_SHARD_PLAN_FILE is required}"
  local count="${TEST_SHARD_COUNT:?TEST_SHARD_COUNT is required}"
  local unknown="${TEST_SHARD_UNKNOWN_DURATION_MS:-60000}"
  [[ "${count}" =~ ^[1-9][0-9]*$ && "${unknown}" =~ ^[1-9][0-9]*$ ]] || fail "Test shard count and unknown duration must be positive integers."
  [ -f "${inventory}" ] || fail "Test inventory is missing: ${inventory}"
  jq -e '
    .schema == "homeboy/test-inventory/v1"
    and (.runner | type == "string" and length > 0)
    and (.runner_fingerprint | type == "string" and length > 0)
    and (.workspace_fingerprint | type == "string" and length > 0)
    and (.inventory_fingerprint | type == "string" and length > 0)
    and (.tests | type == "array" and length > 0)
    and all(.tests[]; (.id | type == "string" and length > 0) and ((.duration_ms? == null) or (.duration_ms | type == "number" and . > 0)))
    and ([.tests[].id] | length == (unique | length))
  ' "${inventory}" >/dev/null || fail "Test inventory must satisfy homeboy/test-inventory/v1."
  [ "${count}" -le "$(jq '.tests | length' "${inventory}")" ] || fail "Test shard count exceeds the test inventory; empty shards are not allowed."

  local canonical inventory_digest plan_digest
  canonical="$(jq -cS '{schema,runner,runner_fingerprint,workspace_fingerprint,inventory_fingerprint,tests:(.tests | sort_by(.id))}' "${inventory}")"
  inventory_digest="$(digest "${canonical}")"
  printf '%s' "${canonical}" | jq -cn --slurpfile inventory /dev/stdin --arg inventory_digest "${inventory_digest}" --argjson count "${count}" --argjson unknown "${unknown}" '
    [range(0; $count) | {index:., duration_ms:0, tests:[]}] as $empty
    | reduce ($inventory[0].tests | map(. + {weight:(.duration_ms // $unknown)}) | sort_by(-.weight, .id))[] as $test
        ($empty; (min_by(.duration_ms, .index).index) as $index | .[$index].duration_ms += $test.weight | .[$index].tests += [$test.id])
    | {schema:"homeboy/test-shard-plan/v1", inventory_digest:$inventory_digest, inventory_fingerprint:$inventory[0].inventory_fingerprint,
       shards:(map({schema:"homeboy/test-shard-manifest/v1", id:("shard-" + ((.index + 1)|tostring)), runner:$inventory[0].runner,
         runner_fingerprint:$inventory[0].runner_fingerprint, workspace_fingerprint:$inventory[0].workspace_fingerprint,
         inventory_fingerprint:$inventory[0].inventory_fingerprint, tests:.tests, estimated_duration_ms:.duration_ms}))}
  ' > "${output}.tmp"
  plan_digest="$(digest "$(jq -cS . "${output}.tmp")")"
  jq --arg digest "${plan_digest}" '. + {plan_digest:$digest}' "${output}.tmp" > "${output}"
  rm -f "${output}.tmp"
}

aggregate() (
  local root="${TEST_SHARD_ARTIFACT_ROOT:?TEST_SHARD_ARTIFACT_ROOT is required}"
  local plan="${TEST_SHARD_PLAN_FILE:?TEST_SHARD_PLAN_FILE is required}"
  local inventory="${TEST_INVENTORY_FILE:?TEST_INVENTORY_FILE is required}"
  local phase="${TEST_SHARD_PHASE:?TEST_SHARD_PHASE is required}"
  local command="${TEST_SHARD_COMMAND:?TEST_SHARD_COMMAND is required}"
  local output="${TEST_SHARD_OUTPUT_DIR:?TEST_SHARD_OUTPUT_DIR is required}"
  local attempt="${RUN_ATTEMPT:?RUN_ATTEMPT is required}"
  local inventory_digest plan_digest canonical
  inventory_digest="$(jq -r .inventory_digest "${plan}")"
  plan_digest="$(jq -r .plan_digest "${plan}")"
  canonical="$(jq -cS '{schema,runner,runner_fingerprint,workspace_fingerprint,inventory_fingerprint,tests:(.tests | sort_by(.id))}' "${inventory}")"
  [ "$(digest "${canonical}")" = "${inventory_digest}" ] || fail "Test inventory does not match this immutable shard plan."
  [ "$(digest "$(jq -cS 'del(.plan_digest)' "${plan}")")" = "${plan_digest}" ] || fail "Test shard plan digest does not match its immutable contents."
  mkdir -p "${output}"

  local shard id manifest payload status aggregate_status="pass" expected_total payload_stream
  payload_stream="$(mktemp)"
  trap 'rm -f -- "${payload_stream:-}"' EXIT
  while IFS= read -r shard; do
    id="$(jq -r .id <<< "${shard}")"; matches=()
    while IFS= read -r manifest; do
      if jq -e --arg phase "${phase}" --arg command "${command}" --arg id "${id}" --arg inventory_digest "${inventory_digest}" --arg plan_digest "${plan_digest}" --argjson attempt "${attempt}" '
        .phase == $phase and .command == $command and .shard_id == $id and .inventory_digest == $inventory_digest and .plan_digest == $plan_digest and .run_attempt == $attempt and (.results[$command] == "pass" or .results[$command] == "fail" or .results[$command] == "timeout")
      ' "${manifest}" >/dev/null 2>&1; then matches+=("${manifest}"); fi
    done < <(find "${root}" -path "*/${phase}/manifest.json" -type f -print | sort)
    [ "${#matches[@]}" -eq 1 ] || fail "Expected exactly one valid ${phase} shard evidence manifest for ${id}."
    manifest="${matches[0]}"; status="$(jq -r --arg command "${command}" '.results[$command]' "${manifest}")"; payload="$(dirname "${manifest}")/homeboy-ci-results/$(command_result_filename "${command}")"
    case "${status}" in
      timeout) aggregate_status="timeout" ;;
      fail) [ "${aggregate_status}" = timeout ] || aggregate_status="fail" ;;
    esac
    if [ ! -f "${payload}" ]; then
      [ "${status}" = timeout ] || fail "${phase} shard ${id} is ${status} but has no structured $(command_result_filename "${command}") result."
      continue
    fi
    expected_total="$(jq '.tests | length' <<< "${shard}")"
    jq -ce --arg root "${command%% *}" --arg phase_status "${status}" '
      if .schema == "homeboy/command-result/v3" and .command == $root and (.success | type == "boolean") and (.status | type == "string") and (.exit_code | type == "number" and floor == .)
        and (if $phase_status == "timeout"
          then .success == false and .status == "failed" and .exit_code == 124
          else (if .success then .status == "succeeded" and .exit_code == 0 else .status == "failed" and .exit_code != 0 end)
            and (.data.test_counts | type == "object") and all(.data.test_counts.passed, .data.test_counts.failed, .data.test_counts.skipped, .data.test_counts.total; type == "number" and . >= 0 and floor == .)
            and (.data.test_counts.passed + .data.test_counts.failed + .data.test_counts.skipped == .data.test_counts.total)
            and (if $phase_status == "pass" then .success == true and .exit_code == 0 and .data.test_counts.failed == 0
                  else .success == false and .exit_code != 0 end)
          end)
      then if $phase_status == "timeout"
        then {data:{test_counts:{passed:0,failed:0,skipped:0,total:0}}}
        else {data:{test_counts:{passed:.data.test_counts.passed,failed:.data.test_counts.failed,skipped:.data.test_counts.skipped,total:.data.test_counts.total}}}
        end
      else empty
      end
    ' "${payload}" >> "${payload_stream}" || fail "${phase} shard ${id} has invalid structured $(command_result_filename "${command}") counts."
    if [ "${status}" = pass ]; then
      [ "$(jq '.data.test_counts.total' "${payload}")" = "${expected_total}" ] || fail "${phase} shard ${id} structured total does not match its planned membership."
    fi
    cp "${payload}" "${output}/${id}-$(command_result_filename "${command}")"
  done < <(jq -c '.shards[]' "${plan}")
  local inventory_total
  inventory_total="$(jq '.tests | length' "${inventory}")"
  if ! jq -n --slurpfile payloads /dev/stdin --arg root "${command%% *}" --arg plan_digest "${plan_digest}" --arg status "${aggregate_status}" --argjson inventory_total "${inventory_total}" '
    {schema:"homeboy/command-result/v3",command:$root,success:($status == "pass"),status:(if $status == "pass" then "succeeded" else "failed" end),exit_code:(if $status == "pass" then 0 elif $status == "timeout" then 124 else 1 end),
     data:{test_counts:{passed:([$payloads[].data.test_counts.passed] | add // 0),failed:([$payloads[].data.test_counts.failed] | add // 0),skipped:([$payloads[].data.test_counts.skipped] | add // 0),total:([$payloads[].data.test_counts.total] | add // 0)},shard_plan_digest:$plan_digest,shard_count:($payloads|length)}}
    | if $status != "pass" or .data.test_counts.total == $inventory_total then . else error("incomplete shard totals") end
  ' < "${payload_stream}" > "${output}/$(command_result_filename "${command}")"; then
    fail "${phase} shard totals do not cover the complete inventory."
  fi
  printf '{"%s":"%s"}\n' "${command}" "${aggregate_status}" > "${output}/results.json"
)

case "${1:-}" in plan) plan ;; aggregate) aggregate ;; *) echo "usage: $0 plan|aggregate" >&2; exit 2 ;; esac
