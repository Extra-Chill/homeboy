#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORK_DIR="$(mktemp -d -t homeboy-rust-shards.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT

PROJECT_DIR="$WORK_DIR/project"
BIN_DIR="$WORK_DIR/bin"
mkdir -p "$PROJECT_DIR/src" "$BIN_DIR" "$WORK_DIR/annotations"
printf '[package]\nname = "shard-smoke"\nversion = "0.1.0"\nedition = "2021"\n' > "$PROJECT_DIR/Cargo.toml"
printf 'pub fn fixture() {}\n' > "$PROJECT_DIR/src/lib.rs"

cat > "$BIN_DIR/test-lib" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'unit::alpha: test' 'unit::beta: test' 'unit::ignored: test'
EOF
cat > "$BIN_DIR/test-api" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'api::works: test'
EOF
chmod +x "$BIN_DIR/test-lib" "$BIN_DIR/test-api"

cat > "$BIN_DIR/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = '--version' ]; then printf 'cargo 1.80.0\n'; exit 0; fi
if [ "${1:-}" = 'nextest' ] && [ "${2:-}" = '--version' ]; then printf 'cargo-nextest 0.9.0\n'; exit 0; fi
if [ "${1:-}" = 'metadata' ]; then
  printf '{"packages":[{"id":"shard-smoke 0.1.0 (path+file:///fixture)","name":"shard-smoke"}],"workspace_root":"%s"}\n' "$PWD"
  exit 0
fi
if [ "${1:-}" = 'nextest' ] && [ "${2:-}" = 'list' ]; then
  printf '%s\n' '{"rust-suites":{"shard-smoke":{"package-name":"shard-smoke","binary-name":"shard_smoke","kind":"lib","testcases":{"unit::alpha":{"ignored":false,"filter-match":{"status":"matches"}},"unit::beta":{"ignored":false,"filter-match":{"status":"matches"}},"unit::ignored":{"ignored":true,"filter-match":{"status":"matches"}},"profile::excluded":{"ignored":false,"filter-match":{"status":"mismatch"}}}},"shard-smoke::api":{"package-name":"shard-smoke","binary-name":"api","kind":"test","testcases":{"api::works":{"ignored":false,"filter-match":{"status":"matches"}}}}}}'
  exit 0
fi
if [[ " $* " == *' --workspace '* && " $* " == *' --no-run '* ]]; then
  printf '{"reason":"compiler-artifact","package_id":"shard-smoke 0.1.0 (path+file:///fixture)","target":{"name":"shard_smoke","kind":["lib"]},"executable":"%s/test-lib"}\n' "$(dirname "$0")"
  printf '{"reason":"compiler-artifact","package_id":"shard-smoke 0.1.0 (path+file:///fixture)","target":{"name":"api","kind":["test"]},"executable":"%s/test-api"}\n' "$(dirname "$0")"
  exit 0
fi
if [[ " $* " == *' --doc '* && " $* " == *' --list '* ]]; then
  exit 0
fi
if [ -n "${HOMEBOY_FAKE_CARGO_LOG:-}" ]; then printf '%s\n' "$*" >> "${HOMEBOY_FAKE_CARGO_LOG}"; fi
if [ "${1:-}" = 'nextest' ] && [ "${2:-}" = 'run' ]; then
  case "${HOMEBOY_NEXTEST_MODE:-pass}" in
    zero) exit 0 ;;
    failure) printf '%s\n' '{"type":"test","name":"shard-smoke::shard_smoke$unit::alpha","event":"ok"}' '{"type":"test","name":"shard-smoke::shard_smoke$unit::beta","event":"failed"}' '{"type":"test","name":"shard-smoke::shard_smoke$unit::ignored","event":"ignored"}' '{"type":"test","name":"shard-smoke::api$api::works","event":"ok"}'; exit 1 ;;
    *) printf '%s\n' '{"type":"test","name":"shard-smoke::shard_smoke$unit::alpha","event":"ok"}' '{"type":"test","name":"shard-smoke::shard_smoke$unit::beta","event":"ok"}' '{"type":"test","name":"shard-smoke::shard_smoke$unit::ignored","event":"ignored"}' '{"type":"test","name":"shard-smoke::api$api::works","event":"ok"}'; exit 0 ;;
  esac
fi
if [ -n "${HOMEBOY_FAIL_TEST:-}" ] && [[ " $* " == *" ${HOMEBOY_FAIL_TEST} "* ]]; then
  printf 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
  exit 1
fi
if [[ " $* " == *' unit::ignored '* ]]; then
  printf 'test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n'
  exit 0
fi
printf 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n'
EOF
chmod +x "$BIN_DIR/cargo"

cat > "$WORK_DIR/runner-prelude.sh" <<'EOF'
homeboy_runner_init() {
  PROJECT_PATH="$HOMEBOY_COMPONENT_PATH"
  EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
  [ -z "${HOMEBOY_RUNTIME_SIDECAR_WRITER:-}" ] || source "$HOMEBOY_RUNTIME_SIDECAR_WRITER"
}
should_run_step() { return 0; }
EOF
cat > "$WORK_DIR/command-capture.sh" <<'EOF'
homeboy_run_step_capture() { local output_var="$1" exit_var="$2"; shift 3; [ "$1" = -- ] && shift; local output status=0; output="$(mktemp)"; "$@" >"$output" 2>&1 || status=$?; printf -v "$output_var" '%s' "$output"; printf -v "$exit_var" '%s' "$status"; return "$status"; }
homeboy_cleanup_step_capture() { rm -f "$1"; }
EOF
cat > "$WORK_DIR/write-test-results.sh" <<'EOF'
homeboy_write_test_results() {
  python3 - "$HOMEBOY_TEST_RESULTS_FILE" "$@" <<'PY'
import json
import sys
path, total, passed, failed, skipped, partial = sys.argv[1:]
with open(path, "w") as handle:
    json.dump({"total": int(total), "passed": int(passed), "failed": int(failed), "skipped": int(skipped), "partial": partial}, handle)
PY
}
EOF
cat > "$WORK_DIR/sidecar-writer.sh" <<'EOF'
homeboy_merge_annotations() { cp "$2" "${HOMEBOY_ANNOTATIONS_DIR}/$1.json"; }
EOF

INVENTORY_A="$WORK_DIR/inventory-a.json"
INVENTORY_B="$WORK_DIR/inventory-b.json"
INVENTORY_NEXTEST="$WORK_DIR/inventory-nextest.json"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$INVENTORY_A" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/inventory-a.out"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$INVENTORY_B" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/inventory-b.out"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$INVENTORY_NEXTEST" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/inventory-nextest.out"

python3 - "$INVENTORY_A" "$INVENTORY_B" "$INVENTORY_NEXTEST" "$WORK_DIR/manifest.json" "$WORK_DIR/nextest-manifest.json" <<'PY'
import json
import sys

first = json.load(open(sys.argv[1]))
second = json.load(open(sys.argv[2]))
nextest = json.load(open(sys.argv[3]))
assert first == second, "inventory must be deterministic"
assert set(first) == {"schema", "runner", "runner_fingerprint", "workspace_fingerprint", "inventory_fingerprint", "tests"}, first
assert first["schema"] == "homeboy/test-inventory/v1", first
assert nextest["schema"] == "homeboy/test-inventory/v1", nextest
assert all(set(test) == {"id", "package", "target", "target_kind", "name"} for test in first["tests"]), first
assert [test["id"] for test in first["tests"]] == [
    "shard-smoke::lib::shard_smoke::unit::alpha",
    "shard-smoke::lib::shard_smoke::unit::beta",
    "shard-smoke::lib::shard_smoke::unit::ignored",
    "shard-smoke::test::api::api::works",
], first
assert nextest["runner"] == "nextest", nextest
assert nextest["runner_fingerprint"] != first["runner_fingerprint"], nextest
assert all("profile::excluded" not in test["id"] for test in nextest["tests"]), nextest
manifest = {
    "schema": "homeboy/test-shard-manifest/v1",
    "runner": first["runner"],
    "inventory_fingerprint": first["inventory_fingerprint"],
    "runner_fingerprint": first["runner_fingerprint"],
    "workspace_fingerprint": first["workspace_fingerprint"],
    "tests": [test["id"] for test in first["tests"]],
}
assert set(manifest) == {"schema", "runner", "inventory_fingerprint", "runner_fingerprint", "workspace_fingerprint", "tests"}, manifest
assert all(isinstance(test_id, str) for test_id in manifest["tests"]), manifest
json.dump(manifest, open(sys.argv[4], "w"))
nextest_manifest = dict(manifest, runner=nextest["runner"], inventory_fingerprint=nextest["inventory_fingerprint"], runner_fingerprint=nextest["runner_fingerprint"], workspace_fingerprint=nextest["workspace_fingerprint"], tests=[test["id"] for test in nextest["tests"]])
json.dump(nextest_manifest, open(sys.argv[5], "w"))
PY

if grep -q 'Running pre-test lint checks' "$WORK_DIR/inventory-a.out" || ! grep -q 'Skipping lint (test inventory only)' "$WORK_DIR/inventory-a.out"; then
  printf 'Expected inventory-only run to bypass pre-test lint\n' >&2
  exit 1
fi

# The runner's sharded branch deliberately uses one exact Cargo invocation per
# identity, even when Cargo's own default test parallelism is configured.
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/manifest.json" \
HOMEBOY_FAKE_CARGO_LOG="$WORK_DIR/cargo.log" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$WORK_DIR/sidecar-writer.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/test-results.json" \
HOMEBOY_ANNOTATIONS_DIR="$WORK_DIR/annotations" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/runner.out"

python3 - "$WORK_DIR/cargo.log" <<'PY'
import sys

lines = open(sys.argv[1]).read().splitlines()
assert len(lines) == 4, lines
assert all(" --exact --test-threads=1" in line for line in lines), lines
assert sum("unit::alpha" in line for line in lines) == 1, lines
assert sum("unit::beta" in line for line in lines) == 1, lines
assert sum("unit::ignored" in line for line in lines) == 1, lines
assert sum("api::works" in line for line in lines) == 1, lines
PY

python3 - "$WORK_DIR/test-results.json" "$WORK_DIR/annotations/rust-test-shard.json" <<'PY'
import json
import sys

results = json.load(open(sys.argv[1]))
assert results == {"total": 4, "passed": 3, "failed": 0, "skipped": 1, "partial": "rust-shard"}, results
record = json.load(open(sys.argv[2]))[0]
assert (record["executed"], record["passed"], record["failed"], record["skipped"]) == (4, 3, 0, 1), record
assert record["duration_ms"] >= 0, record
PY

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_FAKE_CARGO_LOG="$WORK_DIR/cargo.log" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$WORK_DIR/sidecar-writer.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/nextest-test-results.json" \
HOMEBOY_ANNOTATIONS_DIR="$WORK_DIR/annotations" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/nextest-runner.out"

python3 - "$WORK_DIR/cargo.log" <<'PY'
import sys

lines = open(sys.argv[1]).read().splitlines()
nextest = lines[4:]
assert len(nextest) == 1, lines
assert "nextest run" in nextest[0] and " --test-threads 1 " in nextest[0], nextest
assert "test(=unit::alpha)" in nextest[0] and "test(=unit::beta)" in nextest[0] and "test(=api::works)" in nextest[0], nextest
PY

python3 - "$WORK_DIR/nextest-test-results.json" <<'PY'
import json
import sys

assert json.load(open(sys.argv[1])) == {"total": 4, "passed": 3, "failed": 0, "skipped": 1, "partial": "rust-shard"}
PY

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_MODE=zero \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/nextest-zero.out" 2>&1
ZERO_EXIT=$?
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/manifest.json" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" -- unsupported > "$WORK_DIR/passthrough.out" 2>&1
PASSTHROUGH_EXIT=$?
set -e
if [ "$ZERO_EXIT" -eq 0 ] || ! grep -q 'nextest executed membership does not match the shard manifest' "$WORK_DIR/nextest-zero.out"; then
  printf 'Expected nextest zero-selection shard replay to fail closed\n' >&2
  exit 1
fi
if [ "$PASSTHROUGH_EXIT" -eq 0 ] || ! grep -q 'do not support passthrough arguments' "$WORK_DIR/passthrough.out"; then
  printf 'Expected shard replay passthrough rejection\n' >&2
  exit 1
fi

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/manifest.json" \
HOMEBOY_FAIL_TEST='unit::beta' \
HOMEBOY_FAKE_CARGO_LOG="$WORK_DIR/failing-cargo.log" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$WORK_DIR/sidecar-writer.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/failing-test-results.json" \
HOMEBOY_ANNOTATIONS_DIR="$WORK_DIR/annotations" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/failing-runner.out" 2>&1
FAIL_EXIT=$?
set -e
if [ "$FAIL_EXIT" -eq 0 ]; then
  printf 'Expected shard replay to fail at unit::beta\n' >&2
  exit 1
fi

python3 - "$WORK_DIR/failing-test-results.json" "$WORK_DIR/annotations/rust-test-shard.json" <<'PY'
import json
import sys

results = json.load(open(sys.argv[1]))
assert results == {"total": 4, "passed": 2, "failed": 1, "skipped": 1, "partial": "rust-shard"}, results
record = json.load(open(sys.argv[2]))[0]
assert record["status"] == "failed", record
assert (record["total"], record["executed"], record["passed"], record["failed"], record["skipped"]) == (4, 4, 2, 1, 1), record
assert record["duration_ms"] >= 0, record
PY

for invalid in duplicate stale missing runner; do
  python3 - "$WORK_DIR/manifest.json" "$WORK_DIR/$invalid.json" "$invalid" <<'PY'
import json
import sys

manifest = json.load(open(sys.argv[1]))
if sys.argv[3] == "duplicate": manifest["tests"].append(manifest["tests"][0])
if sys.argv[3] == "stale": manifest["workspace_fingerprint"] = "stale"
if sys.argv[3] == "missing": manifest["tests"].append("shard-smoke::missing::nope")
if sys.argv[3] == "runner": manifest["runner"] = "nextest"
json.dump(manifest, open(sys.argv[2], "w"))
PY
  if PATH="$BIN_DIR:$PATH" python3 "$EXTENSION_DIR/scripts/test-shard-inventory.py" --project "$PROJECT_DIR" --runner cargo --output "$WORK_DIR/invalid.out" --manifest "$WORK_DIR/$invalid.json" >/dev/null 2>&1; then
    printf 'Expected %s manifest rejection\n' "$invalid" >&2
    exit 1
  fi
done

printf 'pub fn changed_fixture() {}\n' >> "$PROJECT_DIR/src/lib.rs"
if PATH="$BIN_DIR:$PATH" python3 "$EXTENSION_DIR/scripts/test-shard-inventory.py" --project "$PROJECT_DIR" --runner cargo --output "$WORK_DIR/stale-workspace.out" --manifest "$WORK_DIR/manifest.json" >/dev/null 2>&1; then
  printf 'Expected workspace change to reject the stale manifest\n' >&2
  exit 1
fi

printf 'rust test shard inventory smoke ok\n'
