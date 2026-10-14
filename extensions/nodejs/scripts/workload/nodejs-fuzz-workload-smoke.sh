#!/usr/bin/env bash
# Smoke for the generic `homeboy workload --kind fuzz` adapter (#15840):
# the manifest declaration, the envelope projection of a real Node.js fuzz
# run, and case replay through fuzz-case.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
REPOSITORY_ROOT="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=/dev/null
source "${REPOSITORY_ROOT}/scripts/lib/runtime-helper-resolver.sh"
BASH_PREFLIGHT_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_BASH_PREFLIGHT bash-preflight.sh)" || exit 1
RESOLVE_CONTEXT_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)" || exit 1
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-workload-fuzz.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

node - "${EXTENSION_DIR}/nodejs.json" <<'NODE'
const fs = require('fs');
const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const kind = manifest.workloads?.fuzz;
if (!kind) throw new Error('nodejs.json must declare workloads.fuzz');
if (kind.script !== 'scripts/workload/fuzz-workload.sh') throw new Error('workloads.fuzz.script must be the generic adapter');
if (kind.result_schema !== 'homeboy/fuzz-campaign/v1') throw new Error('workloads.fuzz.result_schema must name the campaign payload');
if (!kind.replay_command?.includes('fuzz-case.sh')) throw new Error('workloads.fuzz.replay_command must run fuzz-case.sh');
if (!kind.compare?.metrics?.some((metric) => metric.name === 'cases_failed')) throw new Error('workloads.fuzz must gate on cases_failed');
NODE

PROJECT="$TMP_DIR/project"
cp -R "${EXTENSION_DIR}/tests/fixtures/workload-fuzz" "$PROJECT"
ARTIFACT_DIR="$TMP_DIR/artifacts"
mkdir -p "$ARTIFACT_DIR"

run_workload() {
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$PROJECT" \
    HOMEBOY_COMPONENT_ID="workload-fuzz-fixture" \
    HOMEBOY_WORKLOAD_KIND=fuzz \
    HOMEBOY_WORKLOAD_ID=package-script-fuzz \
    HOMEBOY_WORKLOAD_RUN_ID=smoke-run \
    HOMEBOY_WORKLOAD_SEED=42 \
    HOMEBOY_WORKLOAD_RESULTS_FILE="$TMP_DIR/envelope.json" \
    HOMEBOY_WORKLOAD_ARTIFACT_DIR="$ARTIFACT_DIR" \
    HOMEBOY_RUNTIME_BASH_PREFLIGHT="$BASH_PREFLIGHT_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
        bash "${SCRIPT_DIR}/fuzz-workload.sh" >/dev/null
}

run_workload
node - "$TMP_DIR/envelope.json" "$ARTIFACT_DIR" <<'NODE'
const fs = require('fs');
const path = require('path');
const [file, artifactDir] = process.argv.slice(2);
const envelope = JSON.parse(fs.readFileSync(file, 'utf8'));
const assert = (condition, message) => { if (!condition) throw new Error(`${message}: ${JSON.stringify(envelope)}`); };
assert(envelope.schema === 'homeboy/workload-result/v1', 'generic envelope schema');
assert(envelope.kind === 'fuzz' && envelope.workload_id === 'package-script-fuzz', 'kind and workload id');
assert(envelope.status === 'fail', 'a campaign with an open finding fails');
assert(envelope.payload.schema === 'homeboy/fuzz-campaign/v1', 'campaign is the payload');
assert(envelope.metrics.cases_total === 2 && envelope.metrics.cases_failed === 1, 'case metrics');
assert(envelope.metrics.findings_open === 1 && envelope.metrics.inputs === 2, 'finding and campaign metrics');
const failed = envelope.cases.find((c) => c.case_id === 'case-empty');
assert(failed.status === 'fail' && failed.seed === '42' && failed.artifact_ref === 'case-empty-input', 'failed case');
const artifact = envelope.artifacts.find((a) => a.name === failed.artifact_ref);
assert(artifact && fs.existsSync(path.join(artifactDir, artifact.path)), 'case artifact exists under the artifact dir');
const finding = envelope.findings[0];
assert(finding.tool === 'fuzz' && finding.fingerprint === 'throw:parse:empty' && finding.severity === 'high', 'finding projection');
assert(finding.metadata.fuzz_status === 'open', 'fuzz triage status rides in metadata');
NODE

printf 'pass' > "$PROJECT/mode"
rm -rf "$ARTIFACT_DIR" && mkdir -p "$ARTIFACT_DIR"
run_workload
node -e "const e=require(process.argv[1]); if (e.status!=='pass'||e.findings.length!==0||e.metrics.cases_failed!==0) throw new Error(JSON.stringify(e))" "$TMP_DIR/envelope.json"

replay_out="$(
    cd "$TMP_DIR" && \
    HOMEBOY_COMPONENT_PATH="$PROJECT" \
    HOMEBOY_WORKLOAD_CASE_MODE=replay \
    HOMEBOY_WORKLOAD_CASE_ID=case-empty \
    HOMEBOY_WORKLOAD_SEED=42 \
    HOMEBOY_WORKLOAD_RUN_ID=smoke-run \
    HOMEBOY_WORKLOAD_CASE_ARTIFACT_FILE="$ARTIFACT_DIR/fuzz/cases/case-empty.json" \
        bash "${SCRIPT_DIR}/fuzz-case.sh"
)"
echo "$replay_out" | grep -q '"case_id":"case-empty"' || { echo "replay did not receive the case env: $replay_out" >&2; exit 1; }
echo "$replay_out" | grep -q '"seed":"42"' || { echo "replay did not receive the seed: $replay_out" >&2; exit 1; }

if HOMEBOY_COMPONENT_PATH="$PROJECT" HOMEBOY_WORKLOAD_CASE_MODE=minimize HOMEBOY_WORKLOAD_CASE_ID=case-empty \
    bash "${SCRIPT_DIR}/fuzz-case.sh" >/dev/null 2>"$TMP_DIR/minimize.err"; then
    echo "minimize without a fuzz:minimize script must fail" >&2
    exit 1
fi
grep -q 'fuzz:minimize' "$TMP_DIR/minimize.err"

echo "nodejs fuzz workload smoke passed"
