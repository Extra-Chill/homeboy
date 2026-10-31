#!/usr/bin/env bash
# Smoke for the generic `homeboy workload --kind fuzz` adapter of the WordPress
# extension (Extra-Chill/homeboy#15840): the manifest declaration, the
# envelope projection of a real fuzz-runner.cjs run over a fixture-only rig
# workload file, and case replay through fuzz-replay.sh.
#
# Runs without WP Codebox: HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH=0 makes the runner
# consume the fixture workload's embedded, fixture-only WP Codebox result.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
FIXTURE="${EXTENSION_DIR}/tests/fixtures/workload-fuzz/rest-fixture.json"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-workload-fuzz.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

node - "${EXTENSION_DIR}/wordpress.json" <<'NODE'
const fs = require('fs');
const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const kind = manifest.workloads?.fuzz;
if (!kind) throw new Error('wordpress.json must declare workloads.fuzz');
if (kind.script !== 'scripts/workload/fuzz-workload.sh') throw new Error('workloads.fuzz.script must be the generic adapter');
if (kind.result_schema !== 'homeboy/fuzz-campaign/v1') throw new Error('workloads.fuzz.result_schema must name the campaign payload');
if (!kind.replay_command?.includes('fuzz-replay.sh')) throw new Error('workloads.fuzz.replay_command must run fuzz-replay.sh');
if (kind.minimize_command) throw new Error('the WordPress runner has no minimizer; do not declare minimize_command');
if (kind.safety_class !== 'read_only') throw new Error('kind-level safety is read_only; destructive workload files escalate per file');
if (!kind.compare?.metrics?.some((metric) => metric.name === 'cases_failed')) throw new Error('workloads.fuzz must gate on cases_failed');
NODE

run_workload() {
    local workload_path="$1" envelope="$2"
    rm -rf "$TMP_DIR/artifacts" && mkdir -p "$TMP_DIR/artifacts"
    HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH=0 \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_WORKLOAD_KIND=fuzz \
    HOMEBOY_WORKLOAD_ID=rest-fixture \
    HOMEBOY_WORKLOAD_RUN_ID=smoke-run \
    HOMEBOY_WORKLOAD_SEED=7 \
    HOMEBOY_WORKLOAD_PATH="$workload_path" \
    HOMEBOY_WORKLOAD_RESULTS_FILE="$envelope" \
    HOMEBOY_WORKLOAD_ARTIFACT_DIR="$TMP_DIR/artifacts" \
        bash "${SCRIPT_DIR}/fuzz-workload.sh" >/dev/null 2>>"$TMP_DIR/runner.err"
}

# 1. Failing campaign: the runner exits non-zero and the envelope says why.
if run_workload "$FIXTURE" "$TMP_DIR/fail.json"; then
    echo "a failing WordPress campaign must exit non-zero" >&2
    exit 1
fi
node - "$TMP_DIR/fail.json" <<'NODE'
const fs = require('fs');
const envelope = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const { payload, ...brief } = envelope;
const assert = (condition, message) => { if (!condition) throw new Error(`${message}: ${JSON.stringify(brief)}`); };
assert(envelope.schema === 'homeboy/workload-result/v1', 'generic envelope schema');
assert(envelope.kind === 'fuzz' && envelope.workload_id === 'rest-fixture', 'kind and workload id');
assert(envelope.status === 'fail', 'a campaign with failing cases fails');
assert(envelope.payload.schema === 'homeboy/fuzz-campaign/v1', 'campaign is the payload');
assert(envelope.metrics.cases_total === 3 && envelope.metrics.cases_failed === 2, 'case metrics');
const byCase = Object.fromEntries(envelope.cases.map((c) => [c.case_id, c]));
assert(byCase['case-list-posts'].status === 'pass', 'passing case');
assert(byCase['case-get-post-404'].status === 'fail' && byCase['case-get-post-404'].seed === 7, 'failed case keeps its seed');
assert(byCase['case-search-slow'].status === 'fail', 'a budget breach fails its case');
const fingerprints = envelope.findings.map((f) => f.fingerprint).sort();
assert(
  JSON.stringify(fingerprints) === JSON.stringify(['GET /wp/v2/posts/{id}:case_failed', 'GET /wp/v2/search:query_count_budget_exceeded:query_count']),
  'findings are fingerprinted by operation, not by generated case id',
);
const budget = envelope.findings.find((f) => f.metadata.kind === 'performance_budget');
assert(budget.metadata.actual === 42 && budget.metadata.budget === 10, 'budget finding carries actual and budget');
for (const artifact of envelope.artifacts) assert(!artifact.path.startsWith('/'), 'artifact paths are relative');
NODE

# 2. Passing campaign: same workload with only the passing case.
node -e "
const fs = require('fs');
const w = JSON.parse(fs.readFileSync(process.argv[1], 'utf8'));
w.wp_codebox_result.status = 'succeeded';
w.wp_codebox_result.cases = w.wp_codebox_result.cases.filter((c) => c.id === 'case-list-posts');
fs.writeFileSync(process.argv[2], JSON.stringify(w));
" "$FIXTURE" "$TMP_DIR/pass-workload.json"
run_workload "$TMP_DIR/pass-workload.json" "$TMP_DIR/pass.json"
node - "$TMP_DIR/pass.json" "$TMP_DIR/artifacts" <<'NODE'
const fs = require('fs');
const path = require('path');
const [file, artifactDir] = process.argv.slice(2);
const e = JSON.parse(fs.readFileSync(file, 'utf8'));
const brief = JSON.stringify({ status: e.status, findings: e.findings, metrics: e.metrics, artifacts: e.artifacts });
if (e.status !== 'pass' || e.findings.length !== 0 || e.metrics.cases_failed !== 0) throw new Error(brief);
if (e.artifacts.length === 0) throw new Error(`a succeeded campaign lists its artifacts: ${brief}`);
for (const artifact of e.artifacts) {
  if (!fs.existsSync(path.join(artifactDir, artifact.path))) throw new Error(`listed artifact missing: ${artifact.path}`);
}
NODE

# 3. No rig workload selected: a clear error, not a silent empty run.
if HOMEBOY_WORKLOAD_RESULTS_FILE="$TMP_DIR/none.json" HOMEBOY_WORKLOAD_ARTIFACT_DIR="$TMP_DIR/none" \
    bash "${SCRIPT_DIR}/fuzz-workload.sh" >/dev/null 2>"$TMP_DIR/none.err"; then
    echo "running without HOMEBOY_WORKLOAD_PATH must fail" >&2
    exit 1
fi
grep -q 'rig workload files' "$TMP_DIR/none.err"

# 4. Replay: still reproduces against the failing workload, passes once fixed.
replay() {
    HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH=0 \
    HOMEBOY_WORKLOAD_KIND=fuzz \
    HOMEBOY_WORKLOAD_CASE_MODE=replay \
    HOMEBOY_WORKLOAD_CASE_ID=case-get-post-404 \
    HOMEBOY_WORKLOAD_SEED=7 \
    HOMEBOY_WORKLOAD_RUN_ID=smoke-run \
    HOMEBOY_WORKLOAD_PATH="$1" \
        bash "${SCRIPT_DIR}/fuzz-replay.sh" 2>/dev/null
}
replay_exit=0
replay_out="$(replay "$FIXTURE")" || replay_exit=$?
[ "$replay_exit" = 1 ] || { echo "replay of a reproducing case must exit 1, got ${replay_exit}: ${replay_out}" >&2; exit 1; }
echo "$replay_out" | grep -q '"status":"fail"' || { echo "replay output: $replay_out" >&2; exit 1; }

node -e "
const fs = require('fs');
const w = JSON.parse(fs.readFileSync(process.argv[1], 'utf8'));
w.wp_codebox_result.status = 'succeeded';
w.wp_codebox_result.cases = w.wp_codebox_result.cases
  .filter((c) => c.id !== 'case-search-slow')
  .map((c) => ({ ...c, status: 'passed', error: undefined }));
fs.writeFileSync(process.argv[2], JSON.stringify(w));
" "$FIXTURE" "$TMP_DIR/fixed-workload.json"
replay "$TMP_DIR/fixed-workload.json" | grep -q '"status":"pass"' || { echo "replay of a fixed case must pass" >&2; exit 1; }

echo "wordpress fuzz workload smoke passed"
