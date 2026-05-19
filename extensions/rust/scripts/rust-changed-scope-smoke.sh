#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

PROJECT_DIR="$WORKDIR/project"
HELPER_DIR="$WORKDIR/helpers"
mkdir -p "$PROJECT_DIR/src/core" "$PROJECT_DIR/tests/core" "$HELPER_DIR"

cat > "$PROJECT_DIR/Cargo.toml" <<'EOF'
[package]
name = "rust-changed-scope-smoke"
version = "0.1.0"
edition = "2021"
EOF

cat > "$PROJECT_DIR/src/lib.rs" <<'EOF'
pub mod core;

pub fn value() -> u8 {
    1
}
EOF

cat > "$PROJECT_DIR/src/core/mod.rs" <<'EOF'
pub mod daemon;
pub mod service;
EOF

cat > "$PROJECT_DIR/src/core/daemon.rs" <<'EOF'
pub fn daemon_value() -> u8 {
    2
}

#[cfg(test)]
#[path = "../../tests/core/daemon_test.rs"]
mod daemon_test;
EOF

cat > "$PROJECT_DIR/tests/integration_scope.rs" <<'EOF'
#[test]
fn integration_scope_runs() {
    assert_eq!(rust_changed_scope_smoke::value(), 1);
}
EOF

cat > "$PROJECT_DIR/tests/core/daemon_test.rs" <<'EOF'
use super::*;

#[test]
fn inline_scope_runs() {
    assert_eq!(daemon_value(), 2);
}
EOF

cat > "$PROJECT_DIR/src/core/service.rs" <<'EOF'
pub fn service_value() -> u8 {
    3
}

#[cfg(test)]
#[path = "../../tests/core/service_test.rs"]
mod service_test;
EOF

cat > "$PROJECT_DIR/tests/core/service_test.rs" <<'EOF'
use super::*;

#[test]
fn second_inline_scope_runs() {
    assert_eq!(service_value(), 3);
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

OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_CHANGED_TEST_FILES='tests/core/daemon_test.rs' \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)

if [[ "$OUTPUT" != *"Scoped to changed files: core::daemon::daemon_test"* ]]; then
    printf 'Expected inline test module scope. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

if [[ "$OUTPUT" != *"1 passed"* ]]; then
    printf 'Expected scoped inline test to run. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_CHANGED_TEST_FILES=$'tests/core/daemon_test.rs\ntests/core/service_test.rs' \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)

if [[ "$OUTPUT" != *"Changed files include multiple inline test modules; running full cargo test."* ]]; then
    printf 'Expected full cargo test fallback for multiple inline filters. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

if [[ "$OUTPUT" != *"2 passed"* ]]; then
    printf 'Expected full fallback to run both inline tests. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi
