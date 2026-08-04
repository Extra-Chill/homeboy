#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RECONCILE="${ROOT}/scripts/core/reconcile-differential-phases.sh"
WORKFLOW="${ROOT}/.github/workflows/ci.yml"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

write_phase() {
  local phase="$1" checkout_sha="$2" component="$3" cli="$4" results="$5" payload="$6"
  local dir="${tmp}/artifacts/${phase}"
  mkdir -p "${dir}/homeboy-ci-results"
  printf '%s\n' "${payload}" > "${dir}/homeboy-ci-results/review-test.json"
  jq -cn --arg phase "${phase}" --arg repository example/repo --arg candidate_sha candidate --arg base_sha base --arg checkout_sha "${checkout_sha}" --arg command 'review test' --arg component "${component}" --arg action_revision action-sha --arg cli_revision "${cli}" --arg binary_sha256 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' --argjson run_attempt 2 --argjson results "${results}" '{phase:$phase,repository:$repository,candidate_sha:$candidate_sha,base_sha:$base_sha,checkout_sha:$checkout_sha,command:$command,component:$component,action_revision:$action_revision,cli_revision:$cli_revision,binary_sha256:$binary_sha256,run_attempt:$run_attempt,results:$results}' > "${dir}/manifest.json"
}

run_case() {
  local expected="$1" label="$2"
  shift 2
  rm -f "${tmp}/output"
  set +e
  PHASE_ARTIFACT_ROOT="${tmp}/artifacts" REPOSITORY=example/repo CANDIDATE_SHA=candidate BASE_SHA=base COMMAND='review test' ACTION_REVISION=action-sha RUN_ATTEMPT=2 REQUIRE_BASELINE=true GITHUB_OUTPUT="${tmp}/output" bash "${RECONCILE}" >/dev/null 2>&1
  local actual=$?
  set -e
  if [ "${actual}" -ne "${expected}" ]; then
    printf 'FAIL: %s (expected exit %s, got %s)\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

payload_pass='{"success":true,"data":{"test_counts":{"failed":0,"errors":0}}}'
payload_one='{"success":false,"data":{"test_counts":{"failed":1,"errors":0}}}'

rm -rf "${tmp}/artifacts"; write_phase candidate candidate component cli '{"review test":"pass"}' "${payload_pass}"; write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
run_case 0 'matching passing phases reconcile green'

rm -rf "${tmp}/artifacts"; write_phase candidate candidate component cli '{"review test":"fail"}' "${payload_one}"; write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
run_case 1 'introduced candidate failure remains blocking'

rm -rf "${tmp}/artifacts"; write_phase candidate candidate component cli '{"review test":"fail"}' "${payload_one}"; write_phase baseline base component cli '{"review test":"fail"}' "${payload_one}"
run_case 0 'baseline red is preserved as a non-regression verdict'
grep -F '"baseline_red"' "${tmp}/output" >/dev/null || { printf 'FAIL: baseline red verdict was not published\n'; exit 1; }

rm -rf "${tmp}/artifacts"; write_phase candidate candidate component cli '{"review test":"timeout"}' "${payload_pass}"; write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
run_case 1 'candidate timeout remains blocking'

rm -rf "${tmp}/artifacts"; mkdir -p "${tmp}/artifacts"; write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
run_case 1 'missing candidate artifact fails closed'

rm -rf "${tmp}/artifacts"; write_phase candidate candidate component-a cli '{"review test":"pass"}' "${payload_pass}"; write_phase baseline base component-b cli '{"review test":"pass"}' "${payload_pass}"
run_case 1 'component provenance mismatch fails closed'

rm -rf "${tmp}/artifacts"; write_phase candidate candidate component cli '{"review test":"pass"}' "${payload_pass}"; write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
jq '.binary_sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"' "${tmp}/artifacts/baseline/manifest.json" > "${tmp}/manifest.json"
mv "${tmp}/manifest.json" "${tmp}/artifacts/baseline/manifest.json"
run_case 1 'binary provenance mismatch fails closed'

for field in repository candidate_sha base_sha command action_revision cli_revision phase; do
  rm -rf "${tmp}/artifacts"
  write_phase candidate candidate component cli '{"review test":"pass"}' "${payload_pass}"
  write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
  jq --arg field "${field}" '.[$field] = "mismatch"' "${tmp}/artifacts/candidate/manifest.json" > "${tmp}/manifest.json"
  mv "${tmp}/manifest.json" "${tmp}/artifacts/candidate/manifest.json"
  run_case 1 "${field} provenance mismatch fails closed"
done

# Candidate and baseline have identical dependencies, so rerunning either job
# does not implicitly rerun the other phase or reuse its checkout.
grep -F '  candidate:' "${WORKFLOW}" >/dev/null
grep -F '  baseline:' "${WORKFLOW}" >/dev/null
grep -F 'needs: [binary, plan]' "${WORKFLOW}" >/dev/null
if grep -A3 '^  baseline:' "${WORKFLOW}" | grep -q 'candidate'; then
  printf 'FAIL: baseline phase depends on candidate and cannot be retried independently\n'
  exit 1
fi
printf 'PASS: workflow keeps candidate and baseline retries independent\n'

if grep -F 'results="${RESULTS:-{}}"' "${WORKFLOW}" >/dev/null \
  || [ "$(grep -Fc 'results="${RESULTS:-}"' "${WORKFLOW}")" -ne 4 ] \
  || [ "$(grep -Fc "[ -n \"\${results}\" ] || results='{}'" "${WORKFLOW}")" -ne 4 ]; then
  printf 'FAIL: candidate and baseline provenance do not preserve populated result JSON\n'
  exit 1
fi
RESULTS='{"review test":"pass"}'
results="${RESULTS:-}"
[ -n "${results}" ] || results='{}'
jq -e '."review test" == "pass"' <<< "${results}" >/dev/null
RESULTS=''
results="${RESULTS:-}"
[ -n "${results}" ] || results='{}'
jq -e 'type == "object" and length == 0' <<< "${results}" >/dev/null
printf 'PASS: populated and missing phase results remain valid provenance JSON\n'

rm -rf "${tmp}/artifacts"
write_phase candidate candidate component cli '{"review test":"pass"}' "${payload_pass}"
write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
mkdir -p "${tmp}/artifacts/prior/candidate"
cp -R "${tmp}/artifacts/candidate/." "${tmp}/artifacts/prior/candidate/"
jq '.run_attempt = 1' "${tmp}/artifacts/prior/candidate/manifest.json" > "${tmp}/manifest.json"
mv "${tmp}/manifest.json" "${tmp}/artifacts/prior/candidate/manifest.json"
run_case 0 'reconciliation reuses the newest independently retried phase artifact'

# These are literal GitHub expression contracts, not shell interpolation.
# shellcheck disable=SC2016
grep -F 'name: ${{ matrix.title }}' "${WORKFLOW}" >/dev/null || { printf 'FAIL: reconciliation did not preserve required check names\n'; exit 1; }
# shellcheck disable=SC2016
grep -F 'differential-gating: ${{ inputs.differential-gating }}' "${WORKFLOW}" >/dev/null || { printf 'FAIL: candidate lost differential scope semantics\n'; exit 1; }
# shellcheck disable=SC2016
grep -F 'scope: ${{ inputs.scope }}' "${WORKFLOW}" >/dev/null || { printf 'FAIL: baseline lost caller scope semantics\n'; exit 1; }
grep -F 'Evaluate PR policy after reconciled quality gates' "${WORKFLOW}" >/dev/null || { printf 'FAIL: reconciled workflow dropped PR policy evaluation\n'; exit 1; }
# shellcheck disable=SC2016
grep -F 'homeboy-differential-candidate-${{ matrix.artifact_key }}-${{ github.run_attempt }}' "${WORKFLOW}" >/dev/null || { printf 'FAIL: candidate artifacts are not command- and attempt-scoped\n'; exit 1; }
printf 'PASS: required checks, scope semantics, and post-reconciliation policy are preserved\n'

# The point of the message is that an operator can act on it. A single
# catch-all ("missing, malformed, or does not match") could not distinguish an
# absent artifact from one wrong field, which made a real reconcile failure in
# Extra-Chill/data-machine#3048 undiagnosable without reproducing the run.
run_case_message() {
  local expected_exit="$1" needle="$2" label="$3"
  rm -f "${tmp}/output"
  set +e
  local out
  out="$(PHASE_ARTIFACT_ROOT="${tmp}/artifacts" REPOSITORY=example/repo CANDIDATE_SHA=candidate BASE_SHA=base COMMAND='review test' ACTION_REVISION=action-sha RUN_ATTEMPT=2 REQUIRE_BASELINE=true GITHUB_OUTPUT="${tmp}/output" bash "${RECONCILE}" 2>&1)"
  local actual=$?
  set -e
  if [ "${actual}" -ne "${expected_exit}" ]; then
    printf 'FAIL: %s (expected exit %s, got %s)\n' "${label}" "${expected_exit}" "${actual}"
    printf '%s\n' "${out}"
    exit 1
  fi
  case "${out}" in
    *"${needle}"*) printf 'PASS: %s\n' "${label}" ;;
    *)
      printf 'FAIL: %s (message did not mention %s)\n' "${label}" "${needle}"
      printf '%s\n' "${out}"
      exit 1
      ;;
  esac
}

rm -rf "${tmp}/artifacts"
write_phase candidate wrong-sha component cli '{"review test":"pass"}' "${payload_pass}"
write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
run_case_message 1 "field 'checkout_sha'" 'a mismatched field is named in the error'
run_case_message 1 "expected 'candidate', artifact has 'wrong-sha'" 'the error reports expected and actual values'

rm -rf "${tmp}/artifacts"
write_phase candidate candidate component cli '{}' "${payload_pass}"
write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
run_case_message 1 "field 'results'" 'an unusable results object is named, not reported as a generic mismatch'

rm -rf "${tmp}/artifacts"
write_phase candidate candidate '' cli '{"review test":"pass"}' "${payload_pass}"
write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
run_case_message 1 "field 'component'" 'an empty component is named'

rm -rf "${tmp}/artifacts"
mkdir -p "${tmp}/artifacts/candidate"
write_phase baseline base component cli '{"review test":"pass"}' "${payload_pass}"
run_case_message 1 "No candidate provenance artifact found" 'a wholly absent candidate artifact reports that it is absent, not silently'

printf 'Reconcile provenance diagnosis checks passed.\n'
