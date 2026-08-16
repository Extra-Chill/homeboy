#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

PROJECT_DIR="$WORKDIR/project"
HELPER_DIR="$WORKDIR/helpers"
BIN_DIR="$WORKDIR/bin"
SIDECAR_DIR="$WORKDIR/sidecars"
RUNNER_PRELUDE_HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-$HELPER_DIR/runner-prelude.sh}"
COMMAND_CAPTURE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-$HELPER_DIR/command-capture.sh}"
mkdir -p "$PROJECT_DIR/tests" "$HELPER_DIR" "$BIN_DIR" "$SIDECAR_DIR"

cat > "$PROJECT_DIR/Cargo.toml" <<'EOF'
[package]
name = "rust-nextest-smoke"
version = "0.1.0"
edition = "2021"
EOF

cat > "$PROJECT_DIR/tests/integration_scope.rs" <<'EOF'
#[test]
fn integration_scope_runs() {
    assert_eq!(1, 1);
}
EOF

cat > "$HELPER_DIR/resolve-context.sh" <<'EOF'
homeboy_resolve_context() {
    PROJECT_PATH="${HOMEBOY_COMPONENT_PATH}"
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
}
EOF

cat > "$HELPER_DIR/runner-prelude.sh" <<'EOF'
homeboy_runner_init() {
    PROJECT_PATH="$HOMEBOY_COMPONENT_PATH"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    [ -z "${HOMEBOY_RUNTIME_SIDECAR_WRITER:-}" ] || source "$HOMEBOY_RUNTIME_SIDECAR_WRITER"
}
should_run_step() { return 0; }
EOF

cat > "$HELPER_DIR/command-capture.sh" <<'EOF'
homeboy_run_step_capture() { local output_var="$1" exit_var="$2"; shift 3; [ "$1" = -- ] && shift; local output status=0; output="$(mktemp)"; "$@" >"$output" 2>&1 || status=$?; printf -v "$output_var" '%s' "$output"; printf -v "$exit_var" '%s' "$status"; return "$status"; }
homeboy_cleanup_step_capture() { rm -f "$1"; }
EOF

cat > "$HELPER_DIR/runner-steps.sh" <<'EOF'
should_run_step() {
    return 0
}
EOF

cat > "$HELPER_DIR/sidecar-writer.sh" <<'EOF'
homeboy_sidecar_merge() {
    local target="$1"
    local source="$2"
    local safe_target="${target//[^A-Za-z0-9_.-]/_}"
    cp "$source" "${HOMEBOY_SIDECAR_DIR}/${safe_target}.json"
}

homeboy_merge_annotations() {
    local target="$1"
    local source="$2"
    local safe_target="annotation.${target}"
    safe_target="${safe_target//[^A-Za-z0-9_.-]/_}"
    cp "$source" "${HOMEBOY_SIDECAR_DIR}/${safe_target}.json"
}
EOF

cat > "$BIN_DIR/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" = "nextest" ] && [ "${2:-}" = "--version" ]; then
    echo "cargo-nextest 0.9.0"
    exit 0
fi

printf '%s\n' "$@" > "${HOMEBOY_FAKE_CARGO_ARGS}"
echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
EOF
chmod +x "$BIN_DIR/cargo"

OUTPUT=$(
    PATH="$BIN_DIR:$PATH" \
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_RUST_TEST_RUNNER=nextest \
    HOMEBOY_TEST_SCOPE_KIND='rust_integration' \
    HOMEBOY_TEST_SCOPE_MESSAGE='Scoped to changed integration tests: integration_scope' \
    HOMEBOY_TEST_RUNNER_ARGS=$'--test\nintegration_scope' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    HOMEBOY_RUNTIME_SIDECAR_WRITER="$HELPER_DIR/sidecar-writer.sh" \
    HOMEBOY_SIDECAR_DIR="$SIDECAR_DIR" \
    HOMEBOY_FAKE_CARGO_ARGS="$WORKDIR/cargo-args.txt" \
    bash "$SCRIPT_DIR/test-runner.sh"
)

if [[ "$OUTPUT" != *"Running cargo nextest"* ]]; then
    printf 'Expected nextest runner selection. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

if [[ "$OUTPUT" != *"Rust test runner: cargo nextest (measured counts from libtest-json-plus)"* ]]; then
    printf 'Expected the measured nextest selection to be stated. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

# An unsharded nextest run is measured by default, so the scope args are
# followed by the flags the libtest-json parser depends on. The scope args must
# still come first and unchanged: measurement is appended to the selection, it
# does not rewrite it.
EXPECTED_ARGS=$'nextest\nrun\n--manifest-path\n'"$PROJECT_DIR"$'/Cargo.toml\n--workspace\n--test\nintegration_scope\n--no-fail-fast\n--no-tests\nwarn\n--message-format\nlibtest-json-plus\n--message-format-version\n0.1'
ACTUAL_ARGS="$(cat "$WORKDIR/cargo-args.txt")"
if [ "$ACTUAL_ARGS" != "$EXPECTED_ARGS" ]; then
    printf 'Expected nextest command shape:\n%s\nActual:\n%s\n' "$EXPECTED_ARGS" "$ACTUAL_ARGS" >&2
    exit 1
fi

if [ -f "$SIDECAR_DIR/test.results.json" ]; then
    printf 'Command plan metadata must not be written to test.results. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

if [ ! -f "$SIDECAR_DIR/annotation.rust-test-plan.json" ]; then
    printf 'Expected rust-test-plan annotation metadata. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

python3 - "$SIDECAR_DIR/annotation.rust-test-plan.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)

record = data[0]
assert record["runner"] == "nextest", record
assert record["scope"] == "rust_integration", record
assert record["args"] == ["--test", "integration_scope"], record
PY

# Measurement off returns the invocation to exactly what it was before measured
# counts existed, so the escape hatch is a real one rather than a renamed
# variant of the new command.
OUTPUT=$(
    PATH="$BIN_DIR:$PATH" \
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_RUST_TEST_RUNNER=nextest \
    HOMEBOY_RUST_NEXTEST_MEASURED_COUNTS=0 \
    HOMEBOY_TEST_SCOPE_KIND='rust_integration' \
    HOMEBOY_TEST_RUNNER_ARGS=$'--test\nintegration_scope' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    HOMEBOY_RUNTIME_SIDECAR_WRITER="$HELPER_DIR/sidecar-writer.sh" \
    HOMEBOY_SIDECAR_DIR="$SIDECAR_DIR" \
    HOMEBOY_FAKE_CARGO_ARGS="$WORKDIR/cargo-args.txt" \
    bash "$SCRIPT_DIR/test-runner.sh"
)

if [[ "$OUTPUT" != *"unmeasured; rust_nextest_measured_counts is off"* ]]; then
    printf 'Expected the disabled measurement to be stated. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

EXPECTED_ARGS=$'nextest\nrun\n--manifest-path\n'"$PROJECT_DIR"$'/Cargo.toml\n--workspace\n--test\nintegration_scope'
ACTUAL_ARGS="$(cat "$WORKDIR/cargo-args.txt")"
if [ "$ACTUAL_ARGS" != "$EXPECTED_ARGS" ]; then
    printf 'Expected legacy nextest command shape:\n%s\nActual:\n%s\n' "$EXPECTED_ARGS" "$ACTUAL_ARGS" >&2
    exit 1
fi

cat > "$BIN_DIR/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" = "nextest" ] && [ "${2:-}" = "--version" ]; then
    exit 1
fi

echo "unexpected cargo invocation: $*" >&2
exit 2
EOF
chmod +x "$BIN_DIR/cargo"

OUTPUT=$(
    PATH="$BIN_DIR:$PATH" \
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_RUST_TEST_RUNNER=nextest \
    HOMEBOY_RUST_NEXTEST_FALLBACK=0 \
    HOMEBOY_TEST_SCOPE_KIND='rust_integration' \
    HOMEBOY_TEST_RUNNER_ARGS=$'--test\nintegration_scope' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh" 2>&1 || true
)

if [[ "$OUTPUT" != *"Error: cargo-nextest requested but not available."* ]]; then
    printf 'Expected actionable missing nextest diagnostic. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

echo "rust nextest runner smoke ok"
