#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT}/.." && pwd)/homeboy}"
CORE_RUNTIME_DIR="${HOMEBOY_CORE_DIR}/crates/homeboy-core/src/extension/runtime"
SIDECAR_WRITER_HELPER="${HOMEBOY_RUNTIME_SIDECAR_WRITER:-${CORE_RUNTIME_DIR}/sidecar-writer.sh}"
RUNNER_PRELUDE_HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${CORE_RUNTIME_DIR}/runner-prelude.sh}"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${CORE_RUNTIME_DIR}/runner-steps.sh}"
COMMAND_CAPTURE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-${CORE_RUNTIME_DIR}/command-capture.sh}"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${CORE_RUNTIME_DIR}/resolve-context.sh}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

if [ ! -f "$SIDECAR_WRITER_HELPER" ]; then
    echo "Missing sidecar writer helper: $SIDECAR_WRITER_HELPER" >&2
    exit 1
fi

assert_json_fields() {
    local file="$1"
    shift
    python3 - "$file" "$@" <<'PY'
import json
import sys

path = sys.argv[1]
fields = sys.argv[2:]
with open(path, encoding="utf-8") as handle:
    data = json.load(handle)
assert isinstance(data, list), data
assert data, data
for field in fields:
    assert field in data[0], (field, data[0])
print("ok", path)
PY
}

# Go lint findings from gofmt.
GO_PROJECT="$TMP_DIR/go-project"
mkdir -p "$GO_PROJECT"
cat > "$GO_PROJECT/go.mod" <<'EOF'
module example.com/sidecar

go 1.22
EOF
cat > "$GO_PROJECT/main.go" <<'EOF'
package main
func main(){println("hi")}
EOF
GO_LINT_FINDINGS="$TMP_DIR/go-lint-findings.json"
set +e
HOMEBOY_COMPONENT_PATH="$GO_PROJECT" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_LINT_FINDINGS_FILE="$GO_LINT_FINDINGS" \
bash "$ROOT/go/scripts/lint-runner.sh" >/dev/null 2>&1
GO_LINT_EXIT=$?
set -e
[ "$GO_LINT_EXIT" -ne 0 ]
assert_json_fields "$GO_LINT_FINDINGS" id file line column severity source code category message fixable fingerprint excerpt

# Go test failures from go test -json.
cat > "$GO_PROJECT/main_test.go" <<'EOF'
package main
import "testing"
func TestSidecarFailure(t *testing.T) { t.Fatal("sidecar failure") }
EOF
GO_TEST_FAILURES="$TMP_DIR/go-test-failures.json"
set +e
HOMEBOY_COMPONENT_PATH="$GO_PROJECT" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_TEST_FAILURES_FILE="$GO_TEST_FAILURES" \
bash "$ROOT/go/scripts/test-runner.sh" >/dev/null 2>&1
GO_TEST_EXIT=$?
set -e
[ "$GO_TEST_EXIT" -ne 0 ]
assert_json_fields "$GO_TEST_FAILURES" test_id suite file line message failure_type fingerprint stdout_excerpt stderr_excerpt

# Rust sidecars with a fake cargo binary so the smoke stays self-contained.
RUST_PROJECT="$TMP_DIR/rust-project"
RUST_BIN="$TMP_DIR/rust-bin"
mkdir -p "$RUST_PROJECT/src" "$RUST_BIN"
cat > "$RUST_PROJECT/Cargo.toml" <<'EOF'
[package]
name = "sidecar"
version = "0.1.0"
edition = "2021"
EOF
cat > "$RUST_PROJECT/src/lib.rs" <<'EOF'
pub fn bad(){ }
EOF
cat > "$RUST_BIN/cargo" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "fmt" ]; then
  printf 'Diff in %s/src/lib.rs at line 1:\n' "$HOMEBOY_COMPONENT_PATH"
  exit 1
fi
if [ "$1" = "test" ]; then
  printf 'running 1 test\n'
  printf 'test tests::sidecar_failure ... FAILED\n'
  printf '\nfailures:\n---- tests::sidecar_failure stdout ----\nthread panicked\n'
  printf 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;\n'
  exit 101
fi
exit 0
EOF
chmod +x "$RUST_BIN/cargo"
RUST_LINT_FINDINGS="$TMP_DIR/rust-lint-findings.json"
set +e
PATH="$RUST_BIN:$PATH" \
HOMEBOY_EXTENSION_PATH="$ROOT/rust" \
HOMEBOY_COMPONENT_PATH="$RUST_PROJECT" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="$RUNNER_STEPS_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_LINT_FINDINGS_FILE="$RUST_LINT_FINDINGS" \
bash "$ROOT/rust/scripts/lint-runner.sh" >/dev/null 2>&1
RUST_LINT_EXIT=$?
set -e
[ "$RUST_LINT_EXIT" -ne 0 ]
assert_json_fields "$RUST_LINT_FINDINGS" id file line column severity source code category message fixable fingerprint excerpt

RUST_TEST_FAILURES="$TMP_DIR/rust-test-failures.json"
set +e
PATH="$RUST_BIN:$PATH" \
HOMEBOY_EXTENSION_PATH="$ROOT/rust" \
HOMEBOY_COMPONENT_PATH="$RUST_PROJECT" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="$RUNNER_STEPS_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_TEST_FAILURES_FILE="$RUST_TEST_FAILURES" \
bash "$ROOT/rust/scripts/test-runner.sh" >/dev/null 2>&1
RUST_TEST_EXIT=$?
set -e
[ "$RUST_TEST_EXIT" -ne 0 ]
assert_json_fields "$RUST_TEST_FAILURES" test_id suite file line message failure_type fingerprint stdout_excerpt stderr_excerpt

# Swift lint and script test sidecars with fake tools.
SWIFT_PROJECT="$TMP_DIR/swift-project"
SWIFT_BIN="$TMP_DIR/swift-bin"
mkdir -p "$SWIFT_PROJECT/tests" "$SWIFT_BIN"
cat > "$SWIFT_PROJECT/tests/Fail.swift" <<'EOF'
fatalError("sidecar")
EOF
cat > "$SWIFT_BIN/swiftlint" <<'EOF'
#!/usr/bin/env bash
printf '[{"file":"%s/tests/Fail.swift","line":1,"character":1,"severity":"Warning","rule_id":"force_unwrapping","reason":"Avoid force unwraps"}]\n' "$HOMEBOY_COMPONENT_PATH"
exit 2
EOF
cat > "$SWIFT_BIN/swift" <<'EOF'
#!/usr/bin/env bash
printf 'Swift script failed\n'
exit 1
EOF
chmod +x "$SWIFT_BIN/swiftlint" "$SWIFT_BIN/swift"
SWIFT_LINT_FINDINGS="$TMP_DIR/swift-lint-findings.json"
set +e
PATH="$SWIFT_BIN:$PATH" \
HOMEBOY_EXTENSION_PATH="$ROOT/swift" \
HOMEBOY_COMPONENT_PATH="$SWIFT_PROJECT" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_LINT_FINDINGS_FILE="$SWIFT_LINT_FINDINGS" \
bash "$ROOT/swift/scripts/lint-runner.sh" >/dev/null 2>&1
SWIFT_LINT_EXIT=$?
set -e
[ "$SWIFT_LINT_EXIT" -ne 0 ]
assert_json_fields "$SWIFT_LINT_FINDINGS" id file line column severity source code category message fixable fingerprint excerpt

SWIFT_TEST_FAILURES="$TMP_DIR/swift-test-failures.json"
set +e
PATH="$SWIFT_BIN:$PATH" \
HOMEBOY_EXTENSION_PATH="$ROOT/swift" \
HOMEBOY_COMPONENT_PATH="$SWIFT_PROJECT" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_TEST_FAILURES_FILE="$SWIFT_TEST_FAILURES" \
bash "$ROOT/swift/scripts/test-runner.sh" >/dev/null 2>&1
SWIFT_TEST_EXIT=$?
set -e
[ "$SWIFT_TEST_EXIT" -ne 0 ]
assert_json_fields "$SWIFT_TEST_FAILURES" test_id suite file line message failure_type fingerprint stdout_excerpt stderr_excerpt

echo "cross-extension sidecar normalization smoke passed"
