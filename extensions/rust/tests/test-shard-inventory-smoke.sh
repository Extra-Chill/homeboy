#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORK_DIR="$(mktemp -d -t homeboy-rust-shards.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT

# The manifest must expose the same whole-byte contract enforced by the runner.
python3 - "$EXTENSION_DIR/rust.json" <<'PY'
import json
import sys

settings = json.load(open(sys.argv[1], encoding="utf-8"))["settings"]
setting = next(item for item in settings if item["id"] == "rust_nextest_filter_max_bytes")
assert setting["type"] == "integer", setting

def accepts(value):
    return isinstance(value, int) and not isinstance(value, bool) and value > 0

assert accepts(setting["default"]), setting
assert accepts(65536)
assert not accepts(65536.5)
PY

PROJECT_DIR="$WORK_DIR/project"
BIN_DIR="$WORK_DIR/bin"
mkdir -p "$PROJECT_DIR/src" "$PROJECT_DIR/member/src" "$BIN_DIR" "$WORK_DIR/annotations"
printf '[package]\nname = "shard-smoke"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\nmembers = ["member"]\n' > "$PROJECT_DIR/Cargo.toml"
printf 'pub fn fixture() {}\n' > "$PROJECT_DIR/src/lib.rs"
printf '[package]\nname = "member-smoke"\nversion = "0.1.0"\nedition = "2021"\n' > "$PROJECT_DIR/member/Cargo.toml"
printf '#[cfg(test)]\nmod tests { #[test] fn member_works() {} }\n' > "$PROJECT_DIR/member/src/lib.rs"

cat > "$BIN_DIR/test-lib" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'unit::alpha: test' 'unit::beta: test' 'unit::ignored: test'
EOF
cat > "$BIN_DIR/test-api" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'api::works: test'
EOF
cat > "$BIN_DIR/test-member" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'member::member_works: test'
EOF
chmod +x "$BIN_DIR/test-lib" "$BIN_DIR/test-api" "$BIN_DIR/test-member"

cat > "$BIN_DIR/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = '--version' ]; then printf 'cargo 1.80.0\n'; exit 0; fi
if [ "${1:-}" = 'nextest' ] && [ "${2:-}" = '--version' ]; then printf 'cargo-nextest 0.9.0\n'; exit 0; fi
if [ "${1:-}" = 'metadata' ]; then
  printf '{"packages":[{"id":"shard-smoke 0.1.0 (path+file:///fixture)","name":"shard-smoke"},{"id":"member-smoke 0.1.0 (path+file:///fixture/member)","name":"member-smoke"}],"workspace_root":"%s"}\n' "$PWD"
  exit 0
fi
if [ "${1:-}" = 'nextest' ] && [ "${2:-}" = 'list' ]; then
  [ -z "${HOMEBOY_FAKE_NEXTEST_LOG:-}" ] || printf 'list %s\n' "$*" >> "$HOMEBOY_FAKE_NEXTEST_LOG"
  # cargo-nextest writes build progress to stderr before its JSON list payload.
  printf '   Compiling shard-smoke v0.1.0\n' >&2
  if [ "${HOMEBOY_NEXTEST_LIST_MODE:-pass}" = stderr-json-only ]; then
    printf '%s\n' '{"rust-suites":{}}' >&2
    printf 'not json\n'
    exit 0
  fi
  if [ "${HOMEBOY_NEXTEST_LIST_MODE:-pass}" = zero ]; then printf '%s\n' '{"rust-suites":{}}'; exit 0; fi
  python3 - "$@" <<'PY'
import json
import os
import re
import sys

count = int(os.environ.get("HOMEBOY_NEXTEST_TEST_COUNT", "5"))
names = ["unit::alpha", "unit::beta", "unit::ignored", "api::works", "member::member_works"] if count == 5 else [f"unit::long_test_{index:04d}_{'x' * 100}" for index in range(count)]
args = sys.argv[1:]
filter_value = args[args.index("-E") + 1] if "-E" in args else ""
selected = set(re.findall(r"test\(=([^)]+)\)", filter_value)) if filter_value else set(names)
suites = {}
for name in names:
    if name not in selected:
        continue
    package, binary, kind = ("shard-smoke", "api", "test") if name == "api::works" else ("member-smoke", "member_smoke", "lib") if name == "member::member_works" else ("shard-smoke", "shard_smoke", "lib")
    suite = suites.setdefault(f"{package}::{binary}", {"package-name": package, "binary-name": binary, "kind": kind, "testcases": {}})
    suite["testcases"][name] = {"ignored": name == "unit::ignored", "filter-match": {"status": "matches"}}
print(json.dumps({"rust-suites": suites}))
PY
  exit 0
fi
if [[ " $* " == *' --workspace '* && " $* " == *' --no-run '* ]]; then
  printf '{"reason":"compiler-artifact","package_id":"shard-smoke 0.1.0 (path+file:///fixture)","target":{"name":"shard_smoke","kind":["lib"]},"executable":"%s/test-lib"}\n' "$(dirname "$0")"
  printf '{"reason":"compiler-artifact","package_id":"shard-smoke 0.1.0 (path+file:///fixture)","target":{"name":"api","kind":["test"]},"executable":"%s/test-api"}\n' "$(dirname "$0")"
  printf '{"reason":"compiler-artifact","package_id":"member-smoke 0.1.0 (path+file:///fixture/member)","target":{"name":"member_smoke","kind":["lib"]},"executable":"%s/test-member"}\n' "$(dirname "$0")"
  exit 0
fi
if [[ " $* " == *' --doc '* && " $* " == *' --list '* ]]; then
  exit 0
fi
if [ -n "${HOMEBOY_FAKE_CARGO_LOG:-}" ]; then printf '%s\n' "$*" >> "${HOMEBOY_FAKE_CARGO_LOG}"; fi
if [ "${1:-}" = 'nextest' ] && [ "${2:-}" = 'run' ]; then
  [ -z "${HOMEBOY_FAKE_NEXTEST_LOG:-}" ] || printf 'run %s\n' "$*" >> "$HOMEBOY_FAKE_NEXTEST_LOG"
  [ -z "${HOMEBOY_FAKE_NEXTEST_RUN_LOG:-}" ] || printf '%s\n' "$*" >> "$HOMEBOY_FAKE_NEXTEST_RUN_LOG"
  python3 - "$@" <<'PY'
import json
import os
import re
import sys

args = sys.argv[1:]
names = re.findall(r"test\(=([^)]+)\)", args[args.index("-E") + 1])
mode = os.environ.get("HOMEBOY_NEXTEST_MODE", "pass")
if mode == "zero":
    raise SystemExit(0)
for name in names:
    package, binary = ("shard-smoke", "api") if name == "api::works" else ("member-smoke", "member_smoke") if name == "member::member_works" else ("shard-smoke", "shard_smoke")
    event = "failed" if mode == "failure" and name == "unit::beta" else "ignored" if name == "unit::ignored" else "ok"
    print(json.dumps({"type": "test", "name": f"{package}::{binary}${name}", "event": event}))
raise SystemExit(1 if mode == "failure" and "unit::beta" in names else 0)
PY
  exit $?
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
homeboy_run_step_capture() {
  local output_var="$1" exit_var="$2" step_name="$3"
  shift 3
  [ "${1:-}" != -- ] || shift
  local output_file
  output_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-command.XXXXXX")"
  set +e
  "$@" 2>&1 | tee "$output_file"
  local command_exit=${PIPESTATUS[0]}
  set -e
  printf -v "$output_var" '%s' "$output_file"
  printf -v "$exit_var" '%s' "$command_exit"
  return "$command_exit"
}
homeboy_cleanup_step_capture() { local output_file="$1"; [ -z "$output_file" ] || rm -f "$output_file"; }
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
    "member-smoke::lib::member_smoke::member::member_works",
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
assert len(lines) == 5, lines
assert all(" --exact --test-threads=1" in line for line in lines), lines
assert sum("unit::alpha" in line for line in lines) == 1, lines
assert sum("unit::beta" in line for line in lines) == 1, lines
assert sum("unit::ignored" in line for line in lines) == 1, lines
assert sum("api::works" in line for line in lines) == 1, lines
assert sum("member::member_works" in line for line in lines) == 1, lines
PY

python3 - "$WORK_DIR/test-results.json" "$WORK_DIR/annotations/rust-test-shard.json" <<'PY'
import json
import sys

results = json.load(open(sys.argv[1]))
assert results == {"total": 5, "passed": 4, "failed": 0, "skipped": 1, "partial": "rust-shard"}, results
record = json.load(open(sys.argv[2]))[0]
assert (record["executed"], record["passed"], record["failed"], record["skipped"]) == (5, 4, 0, 1), record
assert record["duration_ms"] >= 0, record
PY

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_FAKE_CARGO_LOG="$WORK_DIR/cargo.log" \
HOMEBOY_FAKE_NEXTEST_LOG="$WORK_DIR/nextest-selection.log" \
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
nextest = lines[5:]
assert len(nextest) == 1, lines
assert "nextest run" in nextest[0] and " --workspace " in nextest[0] and " --test-threads 1 " in nextest[0], nextest
assert "test(=unit::alpha)" in nextest[0] and "test(=unit::beta)" in nextest[0] and "test(=api::works)" in nextest[0] and "test(=member::member_works)" in nextest[0], nextest
PY

python3 - "$WORK_DIR/nextest-test-results.json" <<'PY'
import json
import sys

assert json.load(open(sys.argv[1])) == {"total": 5, "passed": 4, "failed": 0, "skipped": 1, "partial": "rust-shard"}
PY

python3 - "$WORK_DIR/nextest-selection.log" <<'PY'
import sys

lines = open(sys.argv[1]).read().splitlines()
assert len(lines) == 3, lines
assert lines[1].startswith("list nextest list") and lines[2].startswith("run nextest run"), lines
assert all(" --workspace " in line for line in lines), lines
assert all("test(=member::member_works)" in line for line in lines[1:]), lines
PY

# A small limit forces deterministic batches. Every filter stays below the
# bound while the aggregate result remains the immutable manifest total.
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_RUST_NEXTEST_FILTER_MAX_BYTES=150 \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_FAKE_NEXTEST_RUN_LOG="$WORK_DIR/batched-nextest.log" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$WORK_DIR/sidecar-writer.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/batched-nextest-results.json" \
HOMEBOY_ANNOTATIONS_DIR="$WORK_DIR/annotations" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/batched-nextest.out"

python3 - "$WORK_DIR/batched-nextest.log" "$WORK_DIR/batched-nextest-results.json" <<'PY'
import json
import re
import sys

limit = 150
lines = open(sys.argv[1]).read().splitlines()
assert len(lines) == 5, lines
selected = []
for line in lines:
    filter_value = line.split(" -E ", 1)[1]
    assert len(filter_value.encode()) <= limit, filter_value
    selected.extend(re.findall(r"test\(=([^)]+)\)", filter_value))
assert selected == ["member::member_works", "unit::alpha", "unit::beta", "unit::ignored", "api::works"], selected
assert len(selected) == len(set(selected)), selected
assert json.load(open(sys.argv[2])) == {"total": 5, "passed": 4, "failed": 0, "skipped": 1, "partial": "rust-shard"}
PY

# A nonzero batch exit remains aggregate evidence: every prevalidated batch
# runs exactly once, and the final shard fails only after all outcomes exist.
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_RUST_NEXTEST_FILTER_MAX_BYTES=150 \
HOMEBOY_NEXTEST_MODE=failure \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_FAKE_NEXTEST_RUN_LOG="$WORK_DIR/batched-failure.log" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$WORK_DIR/sidecar-writer.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/batched-failure-results.json" \
HOMEBOY_ANNOTATIONS_DIR="$WORK_DIR/annotations" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/batched-failure.out" 2>&1
BATCH_FAILURE_EXIT=$?
set -e
python3 - "$WORK_DIR/batched-failure.log" "$WORK_DIR/batched-failure-results.json" "$BATCH_FAILURE_EXIT" <<'PY'
import json
import re
import sys

lines = open(sys.argv[1]).read().splitlines()
selected = [name for line in lines for name in re.findall(r"test\(=([^)]+)\)", line)]
assert int(sys.argv[3]) != 0, sys.argv[3]
assert selected == ["member::member_works", "unit::alpha", "unit::beta", "unit::ignored", "api::works"], selected
assert len(selected) == len(set(selected)), selected
assert json.load(open(sys.argv[2])) == {"total": 5, "passed": 3, "failed": 1, "skipped": 1, "partial": "rust-shard"}
PY

# A list validation failure is preflight-only: no batch run may have started.
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_RUST_NEXTEST_FILTER_MAX_BYTES=150 \
HOMEBOY_NEXTEST_LIST_MODE=zero \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_FAKE_NEXTEST_RUN_LOG="$WORK_DIR/zero-list-runs.log" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/zero-list.out" 2>&1
ZERO_LIST_EXIT=$?
set -e
if [ "$ZERO_LIST_EXIT" -eq 0 ] || [ -e "$WORK_DIR/zero-list-runs.log" ]; then
  printf 'Expected zero-list validation to fail before any nextest batch executes\n' >&2
  exit 1
fi

# JSON-looking diagnostics cannot substitute for a missing stdout payload.
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_LIST_MODE=stderr-json-only \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_FAKE_NEXTEST_RUN_LOG="$WORK_DIR/stderr-json-runs.log" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/stderr-json.out" 2>&1
STDERR_JSON_EXIT=$?
set -e
if [ "$STDERR_JSON_EXIT" -eq 0 ] || [ -e "$WORK_DIR/stderr-json-runs.log" ]; then
  printf 'Expected stderr JSON to remain diagnostic-only during nextest preflight\n' >&2
  exit 1
fi

# One identity that cannot fit the configured transport bound fails before list
# or run, rather than attempting an oversized argv.
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_RUST_NEXTEST_FILTER_MAX_BYTES=10 \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/oversized-identity.out" 2>&1
OVERSIZED_EXIT=$?
set -e
if [ "$OVERSIZED_EXIT" -eq 0 ] || ! grep -q 'identity exceeds filter byte limit' "$WORK_DIR/oversized-identity.out"; then
  printf 'Expected individually oversized nextest identity rejection\n' >&2
  exit 1
fi

# This manifest produces a monolithic filter well beyond typical ARG_MAX, but
# bounded batches complete and retain the complete aggregate evidence.
LARGE_INVENTORY="$WORK_DIR/large-nextest-inventory.json"
LARGE_ENVIRONMENT="$(python3 -c 'print("x" * 100000)')"
GETCONF_DIR="$WORK_DIR/getconf-bin"
mkdir -p "$GETCONF_DIR"
cat > "$GETCONF_DIR/getconf" <<'EOF'
#!/usr/bin/env bash
printf '131072\n'
EOF
chmod +x "$GETCONF_DIR/getconf"

# A process with no remaining exec payload fails before either list or run.
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_INHERITED_PADDING="$LARGE_ENVIRONMENT" \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$GETCONF_DIR:$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/no-safe-payload.out" 2>&1
NO_SAFE_PAYLOAD_EXIT=$?
set -e
if [ "$NO_SAFE_PAYLOAD_EXIT" -eq 0 ] || ! grep -q 'no safe nextest filter payload remains' "$WORK_DIR/no-safe-payload.out"; then
  printf 'Expected no-safe-payload nextest filter rejection\n' >&2
  exit 1
fi

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_TEST_COUNT=3000 \
HOMEBOY_INHERITED_PADDING="$LARGE_ENVIRONMENT" \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$LARGE_INVENTORY" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/large-inventory.out"
python3 - "$LARGE_INVENTORY" "$WORK_DIR/large-nextest-manifest.json" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
manifest = {key: inventory[key] for key in ("runner", "inventory_fingerprint", "runner_fingerprint", "workspace_fingerprint")}
manifest["schema"] = "homeboy/test-shard-manifest/v1"
manifest["tests"] = [test["id"] for test in inventory["tests"]]
json.dump(manifest, open(sys.argv[2], "w"))
PY
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_TEST_COUNT=3000 \
HOMEBOY_RUST_NEXTEST_FILTER_MAX_BYTES=999999 \
HOMEBOY_INHERITED_PADDING="$LARGE_ENVIRONMENT" \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/large-nextest-manifest.json" \
HOMEBOY_FAKE_NEXTEST_RUN_LOG="$WORK_DIR/large-nextest.log" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$WORK_DIR/sidecar-writer.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/large-nextest-results.json" \
HOMEBOY_ANNOTATIONS_DIR="$WORK_DIR/annotations" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/large-nextest.out" 2>&1
python3 - "$WORK_DIR/large-nextest.log" "$WORK_DIR/large-nextest-results.json" <<'PY'
import json
import sys

filters = [line.split(" -E ", 1)[1] for line in open(sys.argv[1])]
assert len(filters) > 1, len(filters)
assert all(len(value.encode()) <= 65536 for value in filters), max(map(lambda value: len(value.encode()), filters))
assert json.load(open(sys.argv[2])) == {"total": 3000, "passed": 3000, "failed": 0, "skipped": 0, "partial": "rust-shard"}
PY
if ! grep -q 'filter limit clamped from 999999' "$WORK_DIR/large-nextest.out"; then
  printf 'Expected oversized configured nextest filter limit to clamp\n' >&2
  exit 1
fi

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
assert results == {"total": 5, "passed": 3, "failed": 1, "skipped": 1, "partial": "rust-shard"}, results
record = json.load(open(sys.argv[2]))[0]
assert record["status"] == "failed", record
assert (record["total"], record["executed"], record["passed"], record["failed"], record["skipped"]) == (5, 5, 3, 1, 1), record
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
