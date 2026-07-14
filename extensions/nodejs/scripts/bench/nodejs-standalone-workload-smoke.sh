#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-standalone-workload.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

mkdir -p "$TMP_DIR/helpers" "$TMP_DIR/bin"

cat > "$TMP_DIR/helpers/preflight.sh" <<'EOF'
homeboy_require_bash_version() { :; }
EOF

cat > "$TMP_DIR/helpers/resolve-context.sh" <<'EOF'
homeboy_resolve_context() {
    PROJECT_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
}
EOF

cat > "$TMP_DIR/helpers/bench.sh" <<'EOF'
homeboy_write_empty_bench_results() {
    printf '{"component_id":"%s","iterations":%s,"scenarios":[]}' "$1" "$2" > "$3"
}
EOF

cat > "$TMP_DIR/helpers/bench.mjs" <<'EOF'
import { basename } from 'node:path';
import { writeFile } from 'node:fs/promises';

export function homeboyBenchPercentile(values) {
  return values[0] || 0;
}

export function homeboyBenchScenarioId(file, suffixPattern) {
  return basename(file).replace(suffixPattern, '');
}

export async function homeboyWriteBenchResults(file, componentId, iterations, scenarios) {
  await writeFile(file, JSON.stringify({ component_id: componentId, iterations, scenarios }));
}
EOF

cat > "$TMP_DIR/bin/tsx" <<'EOF'
#!/usr/bin/env bash
exec node "$@"
EOF
chmod +x "$TMP_DIR/bin/tsx"

run_bench() {
    HOMEBOY_RUNTIME_BASH_PREFLIGHT="$TMP_DIR/helpers/preflight.sh" \
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$TMP_DIR/helpers/resolve-context.sh" \
        HOMEBOY_RUNTIME_BENCH_HELPER_SH="$TMP_DIR/helpers/bench.sh" \
        HOMEBOY_RUNTIME_BENCH_HELPER_JS="$TMP_DIR/helpers/bench.mjs" \
        HOMEBOY_COMPONENT_ID="standalone-workload-smoke" \
        HOMEBOY_BENCH_ITERATIONS=1 \
        HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
        HOMEBOY_BENCH_RESULTS_FILE="$1" \
        PATH="$TMP_DIR/bin:$PATH" \
        bash "$SCRIPT_DIR/bench-runner.sh"
}

ANCESTOR_PACKAGE_DIR="$TMP_DIR/ancestor-package"
PROJECT_DIR="$ANCESTOR_PACKAGE_DIR/standalone-project"
mkdir -p "$PROJECT_DIR"
printf '{"name":"unrelated-ancestor-package"}\n' > "$ANCESTOR_PACKAGE_DIR/package.json"
cat > "$PROJECT_DIR/standalone.mjs" <<'EOF'
export default async function () {
  return { metrics: { standalone_runs: 1 } };
}
EOF

RESULTS_FILE="$TMP_DIR/standalone-results.json"
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_BENCH_EXTRA_WORKLOADS="standalone.mjs" \
    run_bench "$RESULTS_FILE" >/dev/null
node -e '
const fs = require("fs");
const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (result.scenarios.length !== 1) throw new Error("standalone workload was not discovered");
if (result.scenarios[0].metrics.standalone_runs !== 1) throw new Error("standalone workload was not executed");
' "$RESULTS_FILE"

ANCESTOR_PACKAGE_RESULTS="$TMP_DIR/ancestor-package-results.json"
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" run_bench "$ANCESTOR_PACKAGE_RESULTS" >/dev/null
node -e '
const fs = require("fs");
const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (result.scenarios.length !== 0) throw new Error("ordinary package mode should use the ancestor package");
' "$ANCESTOR_PACKAGE_RESULTS"

chmod 000 "$PROJECT_DIR/standalone.mjs"
set +e
UNREADABLE_OUTPUT="$(HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" HOMEBOY_BENCH_EXTRA_WORKLOADS="standalone.mjs" run_bench "$TMP_DIR/unreadable-results.json" 2>&1)"
UNREADABLE_EXIT=$?
set -e
chmod 644 "$PROJECT_DIR/standalone.mjs"
if [ "$UNREADABLE_EXIT" -eq 0 ]; then
    echo "expected unreadable standalone workload to fail" >&2
    exit 1
fi
if [[ "$UNREADABLE_OUTPUT" != *'standalone Node.js bench workload must be a readable .mjs file'* ]]; then
    echo "expected standalone workload validation error" >&2
    echo "$UNREADABLE_OUTPUT" >&2
    exit 1
fi

UNPACKAGED_PROJECT_DIR="$TMP_DIR/unpackaged-project"
mkdir -p "$UNPACKAGED_PROJECT_DIR"
set +e
PACKAGE_OUTPUT="$(HOMEBOY_COMPONENT_PATH="$UNPACKAGED_PROJECT_DIR" run_bench "$TMP_DIR/package-results.json" 2>&1)"
PACKAGE_EXIT=$?
set -e
if [ "$PACKAGE_EXIT" -eq 0 ]; then
    echo "expected package validation to fail without a package.json" >&2
    exit 1
fi
if [[ "$PACKAGE_OUTPUT" != *'Not a nodejs project -- cannot run.'* ]]; then
    echo "expected existing package validation error" >&2
    echo "$PACKAGE_OUTPUT" >&2
    exit 1
fi

printf '{"name":"package-backed-bench-smoke"}\n' > "$UNPACKAGED_PROJECT_DIR/package.json"
PACKAGE_RESULTS="$TMP_DIR/package-backed-results.json"
HOMEBOY_COMPONENT_PATH="$UNPACKAGED_PROJECT_DIR" run_bench "$PACKAGE_RESULTS" >/dev/null
node -e '
const fs = require("fs");
const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (result.scenarios.length !== 0) throw new Error("package-backed run should preserve empty-workload behavior");
' "$PACKAGE_RESULTS"

echo "Node.js standalone workload smoke passed."
