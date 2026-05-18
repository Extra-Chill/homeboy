#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

PROJECT_DIR="$WORKDIR/project"
HELPER_DIR="$WORKDIR/helpers"
mkdir -p "$PROJECT_DIR/src" "$PROJECT_DIR/tests" "$HELPER_DIR"

cat > "$PROJECT_DIR/Cargo.toml" <<'EOF'
[package]
name = "rust-changed-scope-smoke"
version = "0.1.0"
edition = "2021"
EOF

cat > "$PROJECT_DIR/src/lib.rs" <<'EOF'
pub fn value() -> u8 {
    1
}
EOF

cat > "$PROJECT_DIR/tests/integration_scope.rs" <<'EOF'
#[test]
fn integration_scope_runs() {
    assert_eq!(rust_changed_scope_smoke::value(), 1);
}
EOF

cat > "$HELPER_DIR/resolve-context.sh" <<'EOF'
homeboy_resolve_context() {
    PROJECT_PATH="${HOMEBOY_COMPONENT_PATH}"
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
}
EOF

cat > "$HELPER_DIR/runner-steps.sh" <<'EOF'
should_run_step() {
    return 0
}
EOF

OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_CHANGED_TEST_FILES='tests/integration_scope.rs' \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)

if [[ "$OUTPUT" != *"Scoped to changed integration tests: integration_scope"* ]]; then
    printf 'Expected integration test target scope. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

if [[ "$OUTPUT" != *"1 passed"* ]]; then
    printf 'Expected scoped integration test to run. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi
