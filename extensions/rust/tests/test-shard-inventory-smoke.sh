#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="${HOMEBOY_TESTED_EXTENSION_DIR:-$(cd "${SCRIPT_DIR}/.." && pwd)}"
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
cat > "$BIN_DIR/test-proc-macro" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'macros::expands: test'
EOF
cat > "$BIN_DIR/test-rlib" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'rlib::works: test'
EOF
cat > "$BIN_DIR/bench-audit-self" <<'EOF'
#!/usr/bin/env bash
if [ -n "${HOMEBOY_BENCH_LIST_LOG:-}" ]; then
  printf 'invoked\n' >> "$HOMEBOY_BENCH_LIST_LOG"
fi
printf '%s\n' "thread 'main' panicked at src/bin/bench-audit-self.rs:44:40:" >&2
printf '%s\n' 'CARGO_MANIFEST_DIR not set (run via cargo): NotPresent' >&2
exit 101
EOF
chmod +x "$BIN_DIR/test-lib" "$BIN_DIR/test-api" "$BIN_DIR/test-member" "$BIN_DIR/test-proc-macro" "$BIN_DIR/test-rlib" "$BIN_DIR/bench-audit-self"

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
if os.environ.get("HOMEBOY_NEXTEST_LIST_MODE") == "filter-escape":
    names.append("unit::closing)")
if os.environ.get("HOMEBOY_NEXTEST_LIST_MODE") == "filter-control":
    names.append("unit::line\nbreak")
args = sys.argv[1:]
filter_value = args[args.index("-E") + 1] if "-E" in args else ""
selected = {value.replace("\\)", ")").replace("\\,", ",").replace("\\\\", "\\") for value in re.findall(r"test\(=((?:\\.|[^)])*)\)", filter_value)} if filter_value else set(names)
suites = {}
for name in names:
    if name not in selected:
        continue
    package, binary, kind = ("shard-smoke", "api", "test") if name == "api::works" else ("member-smoke", "member_smoke", "lib") if name == "member::member_works" else ("shard-smoke", "shard_smoke", "lib")
    # These are the cargo-nextest --message-format json suite and testcase
    # fields consumed by the inventory adapter.
    suite = suites.setdefault(f"{package}::{binary}", {"package-name": package, "binary-name": binary, "kind": kind, "binary-path": f"/fixture/target/debug/deps/{binary}", "build-platform": "target", "cwd": "/fixture", "status": "listed", "testcases": {}})
    suite["testcases"][name] = {"ignored": name == "unit::ignored", "filter-match": {"status": "matches"}}
    if os.environ.get("HOMEBOY_NEXTEST_LIST_MODE") == "ambiguous" and name == "unit::alpha":
        # Distinct planned kind identities can project to the same runtime
        # libtest-json-plus name; that projection must be rejected.
        suites[f"{package}::{binary}::bin"] = {"package-name": package, "binary-name": binary, "kind": "bin", "binary-path": f"/fixture/target/debug/{binary}", "build-platform": "target", "cwd": "/fixture", "status": "listed", "testcases": {name: {"ignored": False, "filter-match": {"status": "matches"}}}}
print(json.dumps({"rust-suites": suites}))
PY
  exit 0
fi
if [[ " $* " == *' --workspace '* && " $* " == *' --no-run '* ]]; then
  printf '{"reason":"compiler-artifact","package_id":"shard-smoke 0.1.0 (path+file:///fixture)","target":{"name":"shard_smoke","kind":["lib"]},"profile":{"test":true},"executable":"%s/test-lib"}\n' "$(dirname "$0")"
  printf '{"reason":"compiler-artifact","package_id":"shard-smoke 0.1.0 (path+file:///fixture)","target":{"name":"api","kind":["test"]},"profile":{"test":true},"executable":"%s/test-api"}\n' "$(dirname "$0")"
  printf '{"reason":"compiler-artifact","package_id":"member-smoke 0.1.0 (path+file:///fixture/member)","target":{"name":"member_smoke","kind":["lib"]},"profile":{"test":true},"executable":"%s/test-member"}\n' "$(dirname "$0")"
  printf '{"reason":"compiler-artifact","package_id":"shard-smoke 0.1.0 (path+file:///fixture)","target":{"name":"macros","kind":["proc-macro"]},"profile":{"test":true},"executable":"%s/test-proc-macro"}\n' "$(dirname "$0")"
  printf '{"reason":"compiler-artifact","package_id":"shard-smoke 0.1.0 (path+file:///fixture)","target":{"name":"rlib_fixture","kind":["rlib"]},"profile":{"test":true},"executable":"%s/test-rlib"}\n' "$(dirname "$0")"
  printf '{"reason":"compiler-artifact","package_id":"shard-smoke 0.1.0 (path+file:///fixture)","target":{"name":"bench-audit-self","kind":["bin"]},"profile":{"test":false},"executable":"%s/bench-audit-self"}\n' "$(dirname "$0")"
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
names = [value.replace("\\)", ")").replace("\\,", ",").replace("\\\\", "\\") for value in re.findall(r"test\(=((?:\\.|[^)])*)\)", args[args.index("-E") + 1])]
mode = os.environ.get("HOMEBOY_NEXTEST_MODE", "pass")
if mode == "zero":
    raise SystemExit(0)
for name in names:
    if mode == "planned-ignored-absent" and name == "unit::ignored":
        continue
    if mode == "missing-terminal" and name == "unit::beta":
        continue
    package, binary = ("shard-smoke", "api") if name == "api::works" else ("member-smoke", "member_smoke") if name == "member::member_works" else ("shard-smoke", "shard_smoke")
    event = "failed" if mode == "failure" and name == "unit::beta" else "ignored" if name == "unit::ignored" else "ok"
    # This is the libtest-json-plus terminal event shape emitted by nextest.
    runtime_name = "malformed" if mode == "malformed" else f"{package}::{binary}${name}"
    print(json.dumps({"type": "test", "name": runtime_name, "event": event, "exec_time": 0.001}))
    if mode == "duplicate-terminal" and name == "unit::alpha":
        print(json.dumps({"type": "test", "name": runtime_name, "event": event, "exec_time": 0.001}))
if mode in {"terminal-outside-ok", "terminal-outside-failed", "terminal-outside-ignored"}:
    event = {"terminal-outside-ok": "ok", "terminal-outside-failed": "failed", "terminal-outside-ignored": "ignored"}[mode]
    print(json.dumps({"type": "test", "name": "outside::suite$terminal", "event": event, "exec_time": 0.001}))
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
BENCH_LIST_LOG="$WORK_DIR/bench-list.log"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$INVENTORY_A" \
HOMEBOY_BENCH_LIST_LOG="$BENCH_LIST_LOG" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/inventory-a.out"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$INVENTORY_B" \
HOMEBOY_BENCH_LIST_LOG="$BENCH_LIST_LOG" \
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

python3 - "$INVENTORY_A" "$INVENTORY_B" "$INVENTORY_NEXTEST" "$WORK_DIR/manifest.json" "$WORK_DIR/nextest-manifest.json" "$WORK_DIR/legacy-nextest-manifest.json" <<'PY'
import hashlib
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
    "shard-smoke::proc-macro::macros::macros::expands",
    "shard-smoke::rlib::rlib_fixture::rlib::works",
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

# This is the exact v1 nextest projection emitted before ignored tests were
# listed: only runnable tests and no expected_outcome metadata participate.
legacy_tests = [{key: test[key] for key in ("id", "package", "target", "target_kind", "name")} for test in nextest["tests"] if test.get("expected_outcome") != "skipped"]
legacy_inventory = {key: nextest[key] for key in ("schema", "runner", "runner_fingerprint", "workspace_fingerprint")}
legacy_inventory["tests"] = legacy_tests
legacy_fingerprint = hashlib.sha256(json.dumps(legacy_inventory, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
legacy_manifest = dict(nextest_manifest, inventory_fingerprint=legacy_fingerprint, tests=[legacy_tests[0]["id"]])
json.dump(legacy_manifest, open(sys.argv[6], "w"))
PY

# A parent v1 manifest may carry only the legacy runnable projection. The
# candidate accepts that exact fingerprint while resolving IDs against its
# richer inventory; tampered legacy selections remain typed rejections.
PATH="$BIN_DIR:$PATH" python3 "$EXTENSION_DIR/scripts/test-shard-inventory.py" --project "$PROJECT_DIR" --runner nextest --output "$WORK_DIR/legacy-nextest-selected.json" --manifest "$WORK_DIR/legacy-nextest-manifest.json"
python3 - "$WORK_DIR/legacy-nextest-selected.json" "$WORK_DIR/legacy-nextest-manifest.json" "$WORK_DIR/legacy-tampered.json" "$WORK_DIR/legacy-stale.json" <<'PY'
import json
import sys

selected = json.load(open(sys.argv[1]))["selected"]
assert len(selected) == 1 and selected[0]["expected_outcome"] == "executed", selected
manifest = json.load(open(sys.argv[2]))
json.dump(dict(manifest, tests=manifest["tests"] + ["shard-smoke::lib::shard_smoke::unit::ignored"]), open(sys.argv[3], "w"))
json.dump(dict(manifest, workspace_fingerprint="stale"), open(sys.argv[4], "w"))
PY
for invalid in "$WORK_DIR/legacy-tampered.json" "$WORK_DIR/legacy-stale.json"; do
  if PATH="$BIN_DIR:$PATH" python3 "$EXTENSION_DIR/scripts/test-shard-inventory.py" --project "$PROJECT_DIR" --runner nextest --output "$WORK_DIR/legacy-invalid.json" --manifest "$invalid" >/dev/null 2>&1; then
    printf 'Expected typed legacy manifest rejection: %s\n' "$invalid" >&2
    exit 1
  fi
done

if [ -e "$BENCH_LIST_LOG" ] || grep -q 'CARGO_MANIFEST_DIR not set' "$WORK_DIR/inventory-a.out"; then
  printf 'Expected non-test bench artifact to remain uninvoked and unreported\n' >&2
  exit 1
fi

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
assert len(lines) == 7, lines
assert all(" --exact --test-threads=1" in line for line in lines), lines
assert sum("unit::alpha" in line for line in lines) == 1, lines
assert sum("unit::beta" in line for line in lines) == 1, lines
assert sum("unit::ignored" in line for line in lines) == 1, lines
assert sum("api::works" in line for line in lines) == 1, lines
assert sum("member::member_works" in line for line in lines) == 1, lines
assert sum("macros::expands" in line for line in lines) == 1, lines
assert sum("rlib::works" in line for line in lines) == 1, lines
assert sum(" --lib " in line and "macros::expands" in line for line in lines) == 1, lines
assert sum(" --lib " in line and "rlib::works" in line for line in lines) == 1, lines
PY

python3 - "$WORK_DIR/test-results.json" "$WORK_DIR/annotations/rust-test-shard.json" <<'PY'
import json
import sys

results = json.load(open(sys.argv[1]))
assert results == {"total": 7, "passed": 6, "failed": 0, "skipped": 1, "partial": "rust-shard"}, results
record = json.load(open(sys.argv[2]))[0]
assert (record["executed"], record["passed"], record["failed"], record["skipped"]) == (7, 6, 0, 1), record
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
nextest = lines[7:]
assert len(nextest) == 1, lines
assert "nextest run" in nextest[0] and " --workspace " in nextest[0] and " --test-threads 1 " in nextest[0], nextest
assert "test(=unit::alpha)" in nextest[0] and "test(=unit::beta)" in nextest[0] and "test(=api::works)" in nextest[0] and "test(=member::member_works)" in nextest[0], nextest
PY

python3 - "$WORK_DIR/nextest-test-results.json" <<'PY'
import json
import sys

assert json.load(open(sys.argv[1])) == {"total": 5, "passed": 4, "failed": 0, "skipped": 1, "partial": "rust-shard"}
PY

# A real libtest-json-plus identity omits `target_kind`; the released replay
# path must reconcile it to the inventory's package/kind/target/test record.
if ! grep -q 'Rust shard result: total=5 passed=4 failed=0 skipped=1' "$WORK_DIR/nextest-runner.out"; then
  printf 'Expected non-empty nextest manifest to execute and report counts\n' >&2
  exit 1
fi

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
assert len(lines) == 4, lines
selected = []
for line in lines:
    filter_value = line.split(" -E ", 1)[1]
    assert len(filter_value.encode()) <= limit, filter_value
    selected.extend(re.findall(r"test\(=([^)]+)\)", filter_value))
assert selected == ["member::member_works", "unit::alpha", "unit::beta", "api::works"], selected
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
assert selected == ["member::member_works", "unit::alpha", "unit::beta", "api::works"], selected
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
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/nextest-zero-results.json" \
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
python3 - "$WORK_DIR/nextest-zero-results.json" <<'PY'
import json
import sys

assert json.load(open(sys.argv[1])) == {"total": 0, "passed": 0, "failed": 0, "skipped": 0, "partial": "rust-shard"}
PY

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_MODE=malformed \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/nextest-malformed-results.json" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/nextest-malformed.out" 2>&1
MALFORMED_EXIT=$?
set -e
if [ "$MALFORMED_EXIT" -eq 0 ] || ! grep -q 'nextest executed membership does not match the shard manifest' "$WORK_DIR/nextest-malformed.out"; then
  printf 'Expected malformed planned terminal stream to fail closed\n' >&2
  exit 1
fi
python3 - "$WORK_DIR/nextest-malformed-results.json" <<'PY'
import json
import sys

assert json.load(open(sys.argv[1])) == {"total": 0, "passed": 0, "failed": 0, "skipped": 0, "partial": "rust-shard"}
PY

assert_nextest_event_rejected() {
  local mode="$1" expected="$2" output
  output="$WORK_DIR/nextest-${mode}.out"
  set +e
  HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
  HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
  HOMEBOY_SKIP_LINT=1 \
  HOMEBOY_RUST_TEST_RUNNER=nextest \
  HOMEBOY_NEXTEST_MODE="$mode" \
  HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
  HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
  HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
  PATH="$BIN_DIR:$PATH" \
  bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$output" 2>&1
  local status=$?
  set -e
  if [ "$status" -eq 0 ] || ! grep -q "$expected" "$output"; then
    printf 'Expected %s nextest terminal event rejection\n' "$mode" >&2
    exit 1
  fi
}

assert_nextest_event_rejected duplicate-terminal 'nextest emitted an unexpected or duplicate test identity'
assert_nextest_event_rejected missing-terminal 'nextest executed membership does not match the shard manifest'

# An ignored helper outside the immutable manifest did not execute. It must not
# be reconciled or counted, while the planned ignored test still counts once.
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_MODE=terminal-outside-ignored \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/ignored-outside-results.json" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/ignored-outside.out"
python3 - "$WORK_DIR/ignored-outside-results.json" <<'PY'
import json
import sys

assert json.load(open(sys.argv[1])) == {"total": 5, "passed": 4, "failed": 0, "skipped": 1, "partial": "rust-shard"}
PY

# nextest list marked unit::ignored as skipped. Default nextest execution does
# not terminalize it, but the immutable manifest still reports one skipped test.
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_MODE=planned-ignored-absent \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/nextest-manifest.json" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/planned-ignored-results.json" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/planned-ignored.out"
python3 - "$WORK_DIR/planned-ignored-results.json" <<'PY'
import json
import sys

assert json.load(open(sys.argv[1])) == {"total": 5, "passed": 4, "failed": 0, "skipped": 1, "partial": "rust-shard"}
PY

make_nextest_manifest() {
  local list_mode="$1" inventory="$2" manifest="$3"
  HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
  HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
  HOMEBOY_RUST_TEST_RUNNER=nextest \
  HOMEBOY_NEXTEST_LIST_MODE="$list_mode" \
  HOMEBOY_TEST_INVENTORY_ONLY=1 \
  HOMEBOY_TEST_INVENTORY_FILE="$inventory" \
  HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
  HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
  PATH="$BIN_DIR:$PATH" \
  bash "$EXTENSION_DIR/scripts/test-runner.sh" >/dev/null
  python3 - "$inventory" "$manifest" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
manifest = {key: inventory[key] for key in ("runner", "inventory_fingerprint", "runner_fingerprint", "workspace_fingerprint")}
manifest["schema"] = "homeboy/test-shard-manifest/v1"
manifest["tests"] = [test["id"] for test in inventory["tests"]]
json.dump(manifest, open(sys.argv[2], "w"))
PY
}

ESCAPED_INVENTORY="$WORK_DIR/escaped-inventory.json"
ESCAPED_MANIFEST="$WORK_DIR/escaped-manifest.json"
make_nextest_manifest filter-escape "$ESCAPED_INVENTORY" "$ESCAPED_MANIFEST"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_LIST_MODE=filter-escape \
HOMEBOY_TEST_SHARD_MANIFEST="$ESCAPED_MANIFEST" \
HOMEBOY_FAKE_NEXTEST_RUN_LOG="$WORK_DIR/escaped-filter.log" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" >/dev/null
if ! grep -q 'test(=unit::closing\\))' "$WORK_DIR/escaped-filter.log"; then
  printf 'Expected nextest filter to escape a closing parenthesis\n' >&2
  exit 1
fi

CONTROL_INVENTORY="$WORK_DIR/control-inventory.json"
CONTROL_MANIFEST="$WORK_DIR/control-manifest.json"
make_nextest_manifest filter-control "$CONTROL_INVENTORY" "$CONTROL_MANIFEST"
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_LIST_MODE=filter-control \
HOMEBOY_TEST_SHARD_MANIFEST="$CONTROL_MANIFEST" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/control-filter.out" 2>&1
CONTROL_EXIT=$?
set -e
if [ "$CONTROL_EXIT" -eq 0 ] || ! grep -q 'nextest filter identity contains a control character' "$WORK_DIR/control-filter.out"; then
  printf 'Expected nextest filter control-character rejection\n' >&2
  exit 1
fi

AMBIGUOUS_INVENTORY="$WORK_DIR/ambiguous-nextest-inventory.json"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_LIST_MODE=ambiguous \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$AMBIGUOUS_INVENTORY" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/ambiguous-inventory.out"
python3 - "$AMBIGUOUS_INVENTORY" "$WORK_DIR/ambiguous-nextest-manifest.json" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
manifest = {key: inventory[key] for key in ("runner", "inventory_fingerprint", "runner_fingerprint", "workspace_fingerprint")}
manifest["schema"] = "homeboy/test-shard-manifest/v1"
manifest["tests"] = [test["id"] for test in inventory["tests"]]
json.dump(manifest, open(sys.argv[2], "w"))
PY
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_NEXTEST_LIST_MODE=ambiguous \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/ambiguous-nextest-manifest.json" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
PATH="$BIN_DIR:$PATH" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" > "$WORK_DIR/nextest-ambiguous.out" 2>&1
AMBIGUOUS_EXIT=$?
set -e
if [ "$AMBIGUOUS_EXIT" -eq 0 ] || ! grep -q 'nextest output identity is ambiguous' "$WORK_DIR/nextest-ambiguous.out"; then
  printf 'Expected ambiguous nextest identity projection to fail closed\n' >&2
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
assert results == {"total": 7, "passed": 5, "failed": 1, "skipped": 1, "partial": "rust-shard"}, results
record = json.load(open(sys.argv[2]))[0]
assert record["status"] == "failed", record
assert (record["total"], record["executed"], record["passed"], record["failed"], record["skipped"]) == (7, 7, 5, 1, 1), record
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
