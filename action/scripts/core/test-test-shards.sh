#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${ROOT}/.github/workflows/ci.yml"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

cat > "${tmp}/inventory.json" <<'JSON'
{"schema":"homeboy/test-inventory/v1","runner":"fixture","runner_fingerprint":"runner-a","workspace_fingerprint":"workspace-a","inventory_fingerprint":"inventory-a","tests":[{"id":"slow","duration_ms":900},{"id":"medium","duration_ms":500},{"id":"unknown"},{"id":"fast","duration_ms":100}]}
JSON

mkdir -p "${tmp}/bin"
cat > "${tmp}/bin/homeboy" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[ "${HOMEBOY_TEST_INVENTORY_ONLY:-}" = 1 ] || exit 99
cp "${FIXTURE_INVENTORY}" "${HOMEBOY_TEST_INVENTORY_FILE}"
SH
chmod +x "${tmp}/bin/homeboy"
PATH="${tmp}/bin:${PATH}" FIXTURE_INVENTORY="${tmp}/inventory.json" HOMEBOY_TEST_INVENTORY_ONLY=1 HOMEBOY_TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" homeboy review test fixture
[ -s "${tmp}/captured-inventory.json" ] || { printf 'FAIL: inventory-only Homeboy contract did not materialize an inventory\n'; exit 1; }

for output in one two; do
  TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PLAN_FILE="${tmp}/${output}.json" TEST_SHARD_COUNT=2 \
    bash "${ROOT}/scripts/core/shard-tests.sh" plan
done
cmp "${tmp}/one.json" "${tmp}/two.json" || { printf 'FAIL: identical inventories must produce byte-identical plans\n'; exit 1; }
jq -e '[.shards[].tests[]] | sort == ["fast","medium","slow","unknown"]' "${tmp}/one.json" >/dev/null || { printf 'FAIL: plan membership is not exact\n'; exit 1; }
if TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PLAN_FILE="${tmp}/too-many.json" TEST_SHARD_COUNT=5 bash "${ROOT}/scripts/core/shard-tests.sh" plan >/dev/null 2>&1; then
  printf 'FAIL: empty shard plans must be rejected\n'; exit 1
fi
jq -e 'all(.shards[]; .schema == "homeboy/test-shard-manifest/v1" and (.runner == "fixture") and (.tests | all(type == "string")))' "${tmp}/one.json" >/dev/null || { printf 'FAIL: emitted shard manifests do not match the extension contract\n'; exit 1; }
jq -n '{schema:"homeboy/test-inventory/v1",runner:"fixture",runner_fingerprint:"runner-a",workspace_fingerprint:"workspace-a",inventory_fingerprint:"large-inventory-a",tests:[range(0; 12000) | {id:("test-" + tostring + ("x" * 160)),duration_ms:1}]}' > "${tmp}/large-inventory.json"
[ "$(wc -c < "${tmp}/large-inventory.json")" -gt 1048576 ] || { printf 'FAIL: large inventory must exceed a conservative argv-sized payload\n'; exit 1; }
for output in large-one large-two; do
  TEST_INVENTORY_FILE="${tmp}/large-inventory.json" TEST_SHARD_PLAN_FILE="${tmp}/${output}.json" TEST_SHARD_COUNT=2 \
    bash "${ROOT}/scripts/core/shard-tests.sh" plan
done
cmp "${tmp}/large-one.json" "${tmp}/large-two.json" || { printf 'FAIL: large inventories must produce byte-identical plans\n'; exit 1; }
jq -e '[.shards[].tests[]] | length == 12000' "${tmp}/large-one.json" >/dev/null || { printf 'FAIL: large plan membership is not exact\n'; exit 1; }
jq '.workspace_fingerprint = "workspace-base"' "${tmp}/captured-inventory.json" > "${tmp}/base-inventory.json"
TEST_INVENTORY_FILE="${tmp}/base-inventory.json" TEST_SHARD_PLAN_FILE="${tmp}/base-plan.json" TEST_SHARD_COUNT=2 bash "${ROOT}/scripts/core/shard-tests.sh" plan
[ "$(jq -r .plan_digest "${tmp}/one.json")" != "$(jq -r .plan_digest "${tmp}/base-plan.json")" ] || { printf 'FAIL: candidate and baseline inventories must produce distinct plans\n'; exit 1; }
printf 'PASS: deterministic duration-balanced plan has exact membership\n'

plan_digest="$(jq -r .plan_digest "${tmp}/one.json")"
inventory_digest="$(jq -r .inventory_digest "${tmp}/one.json")"
for phase in candidate baseline; do
  for shard in shard-1 shard-2; do
    mkdir -p "${tmp}/artifacts/${phase}-${shard}/${phase}/homeboy-ci-results"
    jq -cn --arg phase "${phase}" --arg command 'review test' --arg shard "${shard}" --arg inventory "${inventory_digest}" --arg plan "${plan_digest}" \
      '{phase:$phase,command:$command,shard_id:$shard,inventory_digest:$inventory,plan_digest:$plan,run_attempt:2,results:{"review test":"pass"}}' > "${tmp}/artifacts/${phase}-${shard}/${phase}/manifest.json"
    total="$(jq -r --arg id "${shard}" '.shards[] | select(.id == $id) | .tests | length' "${tmp}/one.json")"
    jq -cn --argjson total "${total}" '{schema:"homeboy/command-result/v3",command:"review",success:true,status:"succeeded",exit_code:0,data:{test_counts:{passed:$total,failed:0,skipped:0,total:$total}}}' > "${tmp}/artifacts/${phase}-${shard}/${phase}/homeboy-ci-results/review-test.json"
  done
  TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE="${phase}" TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/${phase}-output" RUN_ATTEMPT=2 \
    bash "${ROOT}/scripts/core/shard-tests.sh" aggregate
done
jq -e '."review test" == "pass"' "${tmp}/candidate-output/results.json" >/dev/null
jq -e '.data.test_counts.total == 4' "${tmp}/candidate-output/review-test.json" >/dev/null || { printf 'FAIL: aggregation did not sum actual structured shard counts\n'; exit 1; }
cp "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json" "${tmp}/passing-shard.json"
jq '.data.test_counts.failed = 1 | .data.test_counts.passed -= 1' "${tmp}/passing-shard.json" > "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
if TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE=candidate TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/false-pass-output" RUN_ATTEMPT=2 bash "${ROOT}/scripts/core/shard-tests.sh" aggregate >/dev/null 2>&1; then
  printf 'FAIL: passing shard evidence with failed counts must fail closed\n'; exit 1
fi
cp "${tmp}/passing-shard.json" "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
cp "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json" "${tmp}/valid-passing-shard.json"
jq '.status = "failed"' "${tmp}/valid-passing-shard.json" > "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
if TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE=candidate TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/incoherent-status-output" RUN_ATTEMPT=2 bash "${ROOT}/scripts/core/shard-tests.sh" aggregate >/dev/null 2>&1; then
  printf 'FAIL: passing shard evidence with failed status must fail closed\n'; exit 1
fi
jq '.data.test_counts.passed = 0.5 | .data.test_counts.total = 0.5' "${tmp}/valid-passing-shard.json" > "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
if TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE=candidate TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/fractional-count-output" RUN_ATTEMPT=2 bash "${ROOT}/scripts/core/shard-tests.sh" aggregate >/dev/null 2>&1; then
  printf 'FAIL: fractional structured counts must fail closed\n'; exit 1
fi
cp "${tmp}/valid-passing-shard.json" "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
total="$(jq -r '.data.test_counts.total' "${tmp}/passing-shard.json")"
jq --argjson total "$((total - 1))" '.data.test_counts.passed = $total | .data.test_counts.total = $total' "${tmp}/passing-shard.json" > "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
if TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE=candidate TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/incomplete-pass-output" RUN_ATTEMPT=2 bash "${ROOT}/scripts/core/shard-tests.sh" aggregate >/dev/null 2>&1; then
  printf 'FAIL: passing shard evidence with incomplete planned membership must fail closed\n'; exit 1
fi
cp "${tmp}/passing-shard.json" "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
for phase in candidate baseline; do
  jq '.results["review test"] = "fail"' "${tmp}/artifacts/${phase}-shard-1/${phase}/manifest.json" > "${tmp}/bad.json"
  mv "${tmp}/bad.json" "${tmp}/artifacts/${phase}-shard-1/${phase}/manifest.json"
  jq '.success = false | .status = "failed" | .exit_code = 1 | .data.test_counts.failed = 1 | .data.test_counts.passed -= 1' "${tmp}/artifacts/${phase}-shard-1/${phase}/homeboy-ci-results/review-test.json" > "${tmp}/bad.json"
  mv "${tmp}/bad.json" "${tmp}/artifacts/${phase}-shard-1/${phase}/homeboy-ci-results/review-test.json"
  TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE="${phase}" TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/${phase}-failed-output" RUN_ATTEMPT=2 bash "${ROOT}/scripts/core/shard-tests.sh" aggregate
  jq -e '."review test" == "fail"' "${tmp}/${phase}-failed-output/results.json" >/dev/null || { printf 'FAIL: failed shard did not retain a failure result\n'; exit 1; }
done
printf 'PASS: complete provenance-bound shard sets aggregate into Test\n'

jq '.results["review test"] = "fail"' "${tmp}/artifacts/candidate-shard-1/candidate/manifest.json" > "${tmp}/bad.json"
mv "${tmp}/bad.json" "${tmp}/artifacts/candidate-shard-1/candidate/manifest.json"
jq '{schema,command,success:false,status:"failed",exit_code:1,data:{test_counts:{passed:0,failed:0,skipped:0,total:0}}}' "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json" > "${tmp}/bad.json"
mv "${tmp}/bad.json" "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE=candidate TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/zero-count-failed-output" RUN_ATTEMPT=2 bash "${ROOT}/scripts/core/shard-tests.sh" aggregate
jq -e '."review test" == "fail"' "${tmp}/zero-count-failed-output/results.json" >/dev/null || { printf 'FAIL: failed zero-count shard did not retain a failure result\n'; exit 1; }
jq -e '.schema == "homeboy/command-result/v3" and .success == false and .status == "failed" and .exit_code == 1 and (.data.test_counts.passed + .data.test_counts.failed + .data.test_counts.skipped == .data.test_counts.total)' "${tmp}/zero-count-failed-output/review-test.json" >/dev/null || { printf 'FAIL: failed zero-count shard did not emit an aggregate failed v3 result\n'; exit 1; }
cp "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json" "${tmp}/valid-failed-shard.json"
jq '.exit_code = 0' "${tmp}/valid-failed-shard.json" > "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
if TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE=candidate TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/incoherent-exit-output" RUN_ATTEMPT=2 bash "${ROOT}/scripts/core/shard-tests.sh" aggregate >/dev/null 2>&1; then
  printf 'FAIL: failed shard evidence with zero exit code must fail closed\n'; exit 1
fi
cp "${tmp}/valid-failed-shard.json" "${tmp}/artifacts/candidate-shard-1/candidate/homeboy-ci-results/review-test.json"
printf 'PASS: failed zero-count v3 shard aggregates without obsolete errors counts\n'

jq '.results["review test"] = "timeout"' "${tmp}/artifacts/candidate-shard-2/candidate/manifest.json" > "${tmp}/bad.json"
mv "${tmp}/bad.json" "${tmp}/artifacts/candidate-shard-2/candidate/manifest.json"
rm "${tmp}/artifacts/candidate-shard-2/candidate/homeboy-ci-results/review-test.json"
TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE=candidate TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/timeout-output" RUN_ATTEMPT=2 bash "${ROOT}/scripts/core/shard-tests.sh" aggregate
jq -e '."review test" == "timeout"' "${tmp}/timeout-output/results.json" >/dev/null || { printf 'FAIL: timeout without payload must not pass\n'; exit 1; }

jq '.data.test_counts.total = 999' "${tmp}/artifacts/baseline-shard-2/baseline/homeboy-ci-results/review-test.json" > "${tmp}/bad.json"
mv "${tmp}/bad.json" "${tmp}/artifacts/baseline-shard-2/baseline/homeboy-ci-results/review-test.json"
if TEST_SHARD_ARTIFACT_ROOT="${tmp}/artifacts" TEST_SHARD_PLAN_FILE="${tmp}/one.json" TEST_INVENTORY_FILE="${tmp}/captured-inventory.json" TEST_SHARD_PHASE=baseline TEST_SHARD_COMMAND='review test' TEST_SHARD_OUTPUT_DIR="${tmp}/bad-output" RUN_ATTEMPT=2 bash "${ROOT}/scripts/core/shard-tests.sh" aggregate >/dev/null 2>&1; then
  printf 'FAIL: false structured counts must fail closed\n'; exit 1
fi
printf 'PASS: missing, mismatched, failed, and timed-out shards fail closed\n'

grep -F 'needs: [reconcile, reconcile-test-shards, plan]' "${WORKFLOW}" >/dev/null || { printf 'FAIL: policy does not wait for the sharded Test verdict and pinned plan\n'; exit 1; }
grep -F "inputs.test-shards == '1' || needs.reconcile-test-shards.result == 'success'" "${WORKFLOW}" >/dev/null || { printf 'FAIL: policy can bypass a failed sharded Test verdict\n'; exit 1; }
if [ "$(grep -c 'uses: taiki-e/install-action@nextest' "${WORKFLOW}")" -ne 4 ]; then
  printf 'FAIL: Rust candidate and baseline plan/replay jobs do not all install cargo-nextest\n'; exit 1
fi
if [ "$(grep -c "continue-on-error: true" "${WORKFLOW}")" -lt 4 ]; then
  printf 'FAIL: inventory planning transport is not isolated from normal Test verdict enforcement\n'; exit 1
fi
if [ "$(grep -c "HOMEBOY_RUST_NEXTEST_FALLBACK: '0'" "${WORKFLOW}")" -ne 4 ] || [ "$(grep -c 'NEXTEST_PROFILE: ci' "${WORKFLOW}")" -ne 4 ]; then
  printf 'FAIL: Rust shard jobs do not require nextest with the CI profile\n'; exit 1
fi
if [ "$(grep -c 'HOMEBOY_RUST_TEST_RUNNER: nextest' "${WORKFLOW}")" -ne 4 ]; then
  printf 'FAIL: Rust shard jobs do not explicitly select nextest\n'; exit 1
fi
# The literal workflow expression is the contract.
# shellcheck disable=SC2016
grep -F 'baseline_result="$(jq -r --arg command "${COMMAND}"' "${WORKFLOW}" >/dev/null || { printf 'FAIL: differential baseline metadata is not derived from its aggregate result\n'; exit 1; }
printf 'PASS: policy waits for sharded Test reconciliation while preserving unsharded behavior\n'
