#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_DIR}/../.." && pwd)/homeboy}"
REPOSITORY_ROOT="$(cd "${EXTENSION_DIR}/.." && pwd)"
# Helpers resolve through the shared resolver. The literal path used
# here previously pointed at src/core/extension/runtime, a Homeboy layout
# that has not existed since the move to crates/.
# shellcheck source=/dev/null
source "${REPOSITORY_ROOT}/scripts/lib/runtime-helper-resolver.sh"
BASH_PREFLIGHT_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_BASH_PREFLIGHT bash-preflight.sh)" || exit 1
RESOLVE_CONTEXT_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)" || exit 1
MANIFEST="${EXTENSION_DIR}/nodejs.json"
RUNNER="${SCRIPT_DIR}/trace-runner.sh"
HELPER_FIXTURE="${SCRIPT_DIR}/fixtures/helper.trace.mjs"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-trace.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

if [ ! -f "$BASH_PREFLIGHT_HELPER" ]; then
    echo "Missing bash preflight helper: $BASH_PREFLIGHT_HELPER" >&2
    exit 1
fi
if [ ! -f "$RESOLVE_CONTEXT_HELPER" ]; then
    echo "Missing resolve context helper: $RESOLVE_CONTEXT_HELPER" >&2
    exit 1
fi

assert_json() {
    local file="$1"
    local script="$2"
    node -e "const fs = require('fs'); const data = JSON.parse(fs.readFileSync(process.argv[1], 'utf8')); ${script}" "$file"
}

make_project() {
    local name="$1"
    local project_dir="$TMP_DIR/$name"
    mkdir -p "$project_dir"
    printf '{"name":"%s","scripts":{}}\n' "$name" > "$project_dir/package.json"
    printf '%s\n' "$project_dir"
}

run_trace() {
    local project_dir="$1"
    local scenario="$2"
    local results_file="$3"
    local artifact_dir="$4"
    local extra_workloads="${5:-}"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$project_dir" \
    HOMEBOY_COMPONENT_ID="node-trace-smoke" \
    HOMEBOY_TRACE_SCENARIO="$scenario" \
    HOMEBOY_TRACE_RESULTS_FILE="$results_file" \
    HOMEBOY_TRACE_ARTIFACT_DIR="$artifact_dir" \
    HOMEBOY_TRACE_EXTRA_WORKLOADS="$extra_workloads" \
    HOMEBOY_RUN_DIR="$(dirname "$results_file")" \
    HOMEBOY_RUNTIME_BASH_PREFLIGHT="$BASH_PREFLIGHT_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
        bash "$RUNNER"
}

node - "$MANIFEST" "$RUNNER" <<'NODE'
const fs = require('fs');
const [manifestPath, runnerPath] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
const runner = fs.readFileSync(runnerPath, 'utf8');
if (manifest.trace?.extension_script !== 'scripts/trace/trace-runner.sh') {
  throw new Error('trace runner path is not declared in nodejs.json');
}
for (const token of [
  'HOMEBOY_TRACE_RESULTS_FILE',
  'HOMEBOY_TRACE_SCENARIO',
  'HOMEBOY_TRACE_LIST_ONLY',
  'HOMEBOY_TRACE_ARTIFACT_DIR',
  'HOMEBOY_TRACE_EXTRA_WORKLOADS',
  'HOMEBOY_TRACE_HELPER_DIR',
  'HOMEBOY_RUN_DIR',
]) {
  if (!runner.includes(token)) throw new Error(`runner missing env contract token ${token}`);
}
NODE

LIST_PROJECT="$(make_project list-mode)"
mkdir -p "$LIST_PROJECT/traces" "$LIST_PROJECT/scripts/trace"
printf 'console.log("first")\n' > "$LIST_PROJECT/traces/first.trace.mjs"
printf 'console.log("second")\n' > "$LIST_PROJECT/scripts/trace/second.mjs"
EXTRA_TRACE_FILE="$TMP_DIR/extras/third.trace.mjs"
mkdir -p "$(dirname "$EXTRA_TRACE_FILE")"
printf 'console.log("third")\n' > "$EXTRA_TRACE_FILE"
LIST_RESULTS="$TMP_DIR/list-results.json"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$LIST_PROJECT" \
HOMEBOY_COMPONENT_ID="node-trace-smoke" \
HOMEBOY_TRACE_LIST_ONLY="1" \
HOMEBOY_TRACE_RESULTS_FILE="$LIST_RESULTS" \
HOMEBOY_TRACE_EXTRA_WORKLOADS="$EXTRA_TRACE_FILE" \
HOMEBOY_RUNTIME_BASH_PREFLIGHT="$BASH_PREFLIGHT_HELPER" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
    bash "$RUNNER" >/dev/null
assert_json "$LIST_RESULTS" '
if (data.status !== "pass") throw new Error("list envelope did not pass");
const ids = data.scenarios.map((scenario) => scenario.id).sort();
if (ids.join(",") !== "first,second,third") throw new Error(`unexpected scenarios: ${ids.join(",")}`);
if (!data.scenarios.find((scenario) => scenario.id === "first" && scenario.source === "traces/first.trace.mjs")) throw new Error("missing traces/*.trace.mjs source");
if (!data.scenarios.find((scenario) => scenario.id === "second" && scenario.source === "scripts/trace/second.mjs")) throw new Error("missing scripts/trace/*.mjs source");
if (!data.scenarios.find((scenario) => scenario.id === "third" && scenario.source.startsWith("extra:"))) throw new Error("missing extra workload source");
'

TRACE_PROJECT="$(make_project traces-scenario)"
mkdir -p "$TRACE_PROJECT/traces"
cat > "$TRACE_PROJECT/traces/pass.trace.mjs" <<'EOF'
import { writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

mkdirSync(process.env.HOMEBOY_TRACE_ARTIFACT_DIR, { recursive: true });
writeFileSync(join(process.env.HOMEBOY_TRACE_ARTIFACT_DIR, 'trace.txt'), 'artifact');
writeFileSync(process.env.HOMEBOY_TRACE_RESULTS_FILE, JSON.stringify({
  component_id: process.env.HOMEBOY_COMPONENT_ID,
  scenario_id: process.env.HOMEBOY_TRACE_SCENARIO,
  status: 'pass',
  summary: 'custom result',
  timeline: [],
  assertions: [],
  artifacts: [{ label: 'trace', path: 'trace.txt' }],
}, null, 2));
EOF
TRACE_RESULTS="$TMP_DIR/trace-results.json"
TRACE_ARTIFACTS="$TMP_DIR/trace-artifacts"
run_trace "$TRACE_PROJECT" "pass" "$TRACE_RESULTS" "$TRACE_ARTIFACTS" >/dev/null
if [ ! -f "$TRACE_ARTIFACTS/trace.txt" ]; then
    echo "expected scenario artifact to be written" >&2
    exit 1
fi
assert_json "$TRACE_RESULTS" '
if (data.status !== "pass") throw new Error("named traces scenario did not pass");
if (data.summary !== "custom result") throw new Error("runner overwrote scenario result envelope");
if (data.artifacts[0].path !== "trace.txt") throw new Error("artifact path not preserved");
'

HELPER_PROJECT="$(make_project helper-scenario)"
mkdir -p "$HELPER_PROJECT/traces"
cp "$HELPER_FIXTURE" "$HELPER_PROJECT/traces/helper.trace.mjs"
HELPER_RESULTS="$TMP_DIR/helper-results.json"
HELPER_ARTIFACTS="$TMP_DIR/helper-artifacts"
run_trace "$HELPER_PROJECT" "helper" "$HELPER_RESULTS" "$HELPER_ARTIFACTS" >/dev/null
if [ ! -f "$HELPER_ARTIFACTS/process-tree.txt" ]; then
    echo "expected process tree artifact to be written" >&2
    exit 1
fi
if [ ! -f "$HELPER_ARTIFACTS/trace.jsonl" ]; then
    echo "expected timeline jsonl artifact to be written" >&2
    exit 1
fi
if [ ! -f "$HELPER_ARTIFACTS/note.txt" ]; then
    echo "expected recorder artifact to be written" >&2
    exit 1
fi
assert_json "$HELPER_RESULTS" '
if (data.status !== "pass") throw new Error("helper scenario should pass");
if (data.summary !== "helper scenario passed") throw new Error("helper summary missing");
if (!data.timeline.find((event) => event.event === "process.launch")) throw new Error("process launch event missing");
if (!data.timeline.find((event) => event.event === "process.tree.captured")) throw new Error("process tree event missing");
if (!data.timeline.find((event) => event.event === "json.parse_error")) throw new Error("json parse error event missing");
if (!data.timeline.find((event) => event.event === "json.port_known" && event.data.port === 9876)) throw new Error("json port event missing");
if (!data.timeline.find((event) => event.event === "http.first_response" && event.data.status === 502)) throw new Error("http first response event missing");
if (!data.timeline.find((event) => event.event === "http.status" && event.data.status === 302 && event.data.location === "/wp-admin/install.php")) throw new Error("http redirect status transition event missing");
const httpSummary = data.timeline.find((event) => event.event === "http.status_summary");
if (!httpSummary) throw new Error("http status summary event missing");
const statusHistory = httpSummary.data.status_history.map(({ status, count }) => `${status}:${count}`).join(",");
if (statusHistory !== "502:2,302:1,200:1") throw new Error(`unexpected http status history: ${statusHistory}`);
const redirectEntry = httpSummary.data.status_history.find((entry) => entry.status === 302);
if (!redirectEntry || redirectEntry.location !== "/wp-admin/install.php") throw new Error("http redirect location missing from status history");
if (httpSummary.data.repeated_status_count !== 1) throw new Error("http repeated status count missing");
if (httpSummary.data.last_non_ready_status !== 302) throw new Error("http last non-ready status missing");
if (!data.timeline.find((event) => event.event === "http.ready" && event.data.status === 200 && event.data.last_non_ready_status === 302)) throw new Error("http ready event missing summary");
if (!data.timeline.find((event) => event.event === "process.seen")) throw new Error("process seen event missing");
if (!data.timeline.find((event) => event.event === "log.port_known" && event.data.port === 1234)) throw new Error("log parse event missing");
if (!data.timeline.find((event) => event.event === "console.bridge" && event.data.event === "bridge-ok")) throw new Error("console bridge event missing");
if (!data.timeline.find((event) => event.source === "renderer" && event.event === "site_event_received" && event.data.running === true)) throw new Error("structured console bridge event missing");
if (!data.timeline.find((event) => event.source === "ipc" && event.event === "createSite.resolve" && event.data.result.port === 9999)) throw new Error("prefixed trace line event missing");
if (!data.assertions.find((assertion) => assertion.id === "dummy-process-exited" && assertion.status === "pass")) throw new Error("process assertion missing");
if (!data.assertions.find((assertion) => assertion.id === "record-check-helper" && assertion.status === "pass")) throw new Error("recordCheck assertion missing");
if (!data.assertions.find((assertion) => assertion.id === "json-poll-port-known" && assertion.status === "pass")) throw new Error("json poll assertion missing");
if (!data.assertions.find((assertion) => assertion.id === "http-poll-ready" && assertion.status === "pass")) throw new Error("http poll assertion missing");
if (!data.assertions.find((assertion) => assertion.id === "http-status-history" && assertion.status === "pass")) throw new Error("http status history assertion missing");
if (!data.assertions.find((assertion) => assertion.id === "http-status-history-helper" && assertion.status === "pass")) throw new Error("http status history helper assertion missing");
if (!data.assertions.find((assertion) => assertion.id === "process-poll-seen" && assertion.status === "pass")) throw new Error("process poll assertion missing");
if (!data.assertions.find((assertion) => assertion.id === "observation-window-resolved" && assertion.status === "pass")) throw new Error("observation window assertion missing");
if (!data.artifacts.find((artifact) => artifact.path === "process-tree.txt")) throw new Error("process tree artifact missing from envelope");
if (!data.artifacts.find((artifact) => artifact.path === "trace.jsonl")) throw new Error("timeline artifact missing from envelope");
if (!data.artifacts.find((artifact) => artifact.path === "note.txt" && artifact.kind === "text/plain")) throw new Error("recorder-written artifact missing from envelope");
'

SCRIPT_PROJECT="$(make_project scripts-scenario)"
mkdir -p "$SCRIPT_PROJECT/scripts/trace"
cat > "$SCRIPT_PROJECT/scripts/trace/fallback.mjs" <<'EOF'
import { writeFileSync } from 'node:fs';
writeFileSync(process.env.HOMEBOY_TRACE_RESULTS_FILE, JSON.stringify({
  component_id: process.env.HOMEBOY_COMPONENT_ID,
  scenario_id: process.env.HOMEBOY_TRACE_SCENARIO,
  status: 'pass',
  summary: 'fallback result',
  timeline: [],
  assertions: [],
  artifacts: [],
}, null, 2));
EOF
SCRIPT_RESULTS="$TMP_DIR/script-results.json"
run_trace "$SCRIPT_PROJECT" "fallback" "$SCRIPT_RESULTS" "$TMP_DIR/script-artifacts" >/dev/null
assert_json "$SCRIPT_RESULTS" '
if (data.status !== "pass") throw new Error("scripts/trace fallback did not pass");
if (data.summary !== "fallback result") throw new Error("scripts/trace result missing");
'

EXTRA_PROJECT="$(make_project extra-scenario)"
EXTRA_WORKLOAD="$TMP_DIR/extra-workloads/rig-only.mjs"
mkdir -p "$(dirname "$EXTRA_WORKLOAD")"
cat > "$EXTRA_WORKLOAD" <<'EOF'
import { writeFileSync } from 'node:fs';
writeFileSync(process.env.HOMEBOY_TRACE_RESULTS_FILE, JSON.stringify({
  component_id: process.env.HOMEBOY_COMPONENT_ID,
  scenario_id: process.env.HOMEBOY_TRACE_SCENARIO,
  status: 'pass',
  summary: 'extra workload result',
  timeline: [],
  assertions: [],
  artifacts: [],
}, null, 2));
EOF
EXTRA_RESULTS="$TMP_DIR/extra-results.json"
run_trace "$EXTRA_PROJECT" "rig-only" "$EXTRA_RESULTS" "$TMP_DIR/extra-artifacts" "$EXTRA_WORKLOAD" >/dev/null
assert_json "$EXTRA_RESULTS" '
if (data.status !== "pass") throw new Error("extra workload scenario did not pass");
if (data.summary !== "extra workload result") throw new Error("extra workload result missing");
'

PRECEDENCE_PROJECT="$(make_project precedence-scenario)"
mkdir -p "$PRECEDENCE_PROJECT/traces"
cat > "$PRECEDENCE_PROJECT/traces/dupe.trace.mjs" <<'EOF'
import { writeFileSync } from 'node:fs';
writeFileSync(process.env.HOMEBOY_TRACE_RESULTS_FILE, JSON.stringify({
  component_id: process.env.HOMEBOY_COMPONENT_ID,
  scenario_id: process.env.HOMEBOY_TRACE_SCENARIO,
  status: 'pass',
  summary: 'local project result',
  timeline: [],
  assertions: [],
  artifacts: [],
}, null, 2));
EOF
EXTRA_DUPE="$TMP_DIR/extra-workloads/dupe.trace.mjs"
cat > "$EXTRA_DUPE" <<'EOF'
throw new Error('extra workload should not run when a local scenario has the same id');
EOF
PRECEDENCE_RESULTS="$TMP_DIR/precedence-results.json"
run_trace "$PRECEDENCE_PROJECT" "dupe" "$PRECEDENCE_RESULTS" "$TMP_DIR/precedence-artifacts" "$EXTRA_DUPE" >/dev/null
assert_json "$PRECEDENCE_RESULTS" '
if (data.status !== "pass") throw new Error("duplicate-id local scenario did not pass");
if (data.summary !== "local project result") throw new Error("local project scenario did not win over extra workload");
'

NPM_PROJECT="$(make_project npm-scenario)"
cat > "$NPM_PROJECT/package.json" <<'EOF'
{"name":"npm-scenario","scripts":{"trace":"node trace-script.mjs"}}
EOF
cat > "$NPM_PROJECT/trace-script.mjs" <<'EOF'
import { writeFileSync } from 'node:fs';
const scenario = process.argv.at(-1);
writeFileSync(process.env.HOMEBOY_TRACE_RESULTS_FILE, JSON.stringify({
  component_id: process.env.HOMEBOY_COMPONENT_ID,
  scenario_id: scenario,
  status: 'pass',
  summary: 'npm trace result',
  timeline: [],
  assertions: [],
  artifacts: [],
}, null, 2));
EOF
NPM_RESULTS="$TMP_DIR/npm-results.json"
run_trace "$NPM_PROJECT" "npm-only" "$NPM_RESULTS" "$TMP_DIR/npm-artifacts" >/dev/null
assert_json "$NPM_RESULTS" '
if (data.status !== "pass") throw new Error("npm trace fallback did not pass");
if (data.scenario_id !== "npm-only") throw new Error("npm trace did not receive scenario id");
'

MISSING_PROJECT="$(make_project missing-scenario)"
MISSING_RESULTS="$TMP_DIR/missing-results.json"
set +e
MISSING_OUTPUT="$(run_trace "$MISSING_PROJECT" "missing" "$MISSING_RESULTS" "$TMP_DIR/missing-artifacts" 2>&1)"
MISSING_EXIT=$?
set -e
if [ "$MISSING_EXIT" -eq 0 ]; then
    echo "expected missing scenario to fail" >&2
    exit 1
fi
if [[ "$MISSING_OUTPUT" != *"Trace scenario not found: missing"* ]]; then
    echo "expected clear missing scenario error" >&2
    echo "$MISSING_OUTPUT" >&2
    exit 1
fi
assert_json "$MISSING_RESULTS" '
if (data.status !== "error") throw new Error("missing scenario should write error envelope");
if (!data.failure.includes("traces/missing.trace.mjs")) throw new Error("missing scenario failure lacks searched paths");
'

FAIL_PROJECT="$(make_project failing-scenario)"
mkdir -p "$FAIL_PROJECT/traces"
cat > "$FAIL_PROJECT/traces/fail.trace.mjs" <<'EOF'
process.exit(7);
EOF
FAIL_RESULTS="$TMP_DIR/fail-results.json"
set +e
FAIL_OUTPUT="$(run_trace "$FAIL_PROJECT" "fail" "$FAIL_RESULTS" "$TMP_DIR/fail-artifacts" 2>&1)"
FAIL_EXIT=$?
set -e
if [ "$FAIL_EXIT" -ne 7 ]; then
    echo "expected failing scenario exit 7, got $FAIL_EXIT" >&2
    echo "$FAIL_OUTPUT" >&2
    exit 1
fi
assert_json "$FAIL_RESULTS" '
if (data.status !== "error") throw new Error("non-zero scenario should write error envelope");
if (!data.failure.includes("exited with code 7")) throw new Error("non-zero failure summary missing exit code");
'

echo "Node.js trace runner smoke passed."
