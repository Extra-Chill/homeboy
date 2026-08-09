#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

PROJECT_DIR="$WORKDIR/project"
HELPER_DIR="$WORKDIR/helpers"
RUNNER_PRELUDE_HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-$HELPER_DIR/runner-prelude.sh}"
COMMAND_CAPTURE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-$HELPER_DIR/command-capture.sh}"
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

cat > "$HELPER_DIR/runner-prelude.sh" <<'EOF'
homeboy_runner_init() {
    PROJECT_PATH="$HOMEBOY_COMPONENT_PATH"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
should_run_step() { return 0; }
EOF

cat > "$HELPER_DIR/command-capture.sh" <<'EOF'
homeboy_run_step_capture() { local output_var="$1" exit_var="$2"; shift 3; [ "$1" = -- ] && shift; local output status=0; output="$(mktemp)"; "$@" 2>&1 | tee "$output"; status=${PIPESTATUS[0]}; printf -v "$output_var" '%s' "$output"; printf -v "$exit_var" '%s' "$status"; return "$status"; }
homeboy_cleanup_step_capture() { rm -f "$1"; }
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
    HOMEBOY_TEST_SCOPE_KIND='rust_integration' \
    HOMEBOY_TEST_SCOPE_MESSAGE='Scoped to changed integration tests: integration_scope' \
    HOMEBOY_TEST_RUNNER_ARGS=$'--test\nintegration_scope' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
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
    HOMEBOY_TEST_SCOPE_KIND='rust_filter' \
    HOMEBOY_TEST_SCOPE_MESSAGE='Scoped to changed files: core::daemon::daemon_test' \
    HOMEBOY_TEST_RUNNER_ARGS=$'--\ncore::daemon::daemon_test' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
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

cat > "$WORKDIR/changed-selection.json" <<'EOF'
{"schema":"homeboy/rust-changed-test-selection/v1","candidates":[{"package":"rust-changed-scope-smoke","target_kind":"lib","target":"rust_changed_scope_smoke","module":"core::daemon::daemon_test"},{"package":"rust-changed-scope-smoke","target_kind":"test","target":"integration_scope","module":null}]}
EOF
OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_TEST_SCOPE_KIND='rust_changed_union' \
    HOMEBOY_RUST_CHANGED_TEST_SELECTION_FILE="$WORKDIR/changed-selection.json" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)
if [[ "$OUTPUT" != *"inline_scope_runs ... ok"* || "$OUTPUT" != *"integration_scope_runs ... ok"* || "$OUTPUT" == *"second_inline_scope_runs ... ok"* ]]; then
    printf 'Expected exact mixed changed-scope union membership. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

OUTPUT=$( 
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_RUST_TEST_RUNNER=nextest \
    HOMEBOY_RUST_NEXTEST_FILTER_MAX_BYTES=180 \
    HOMEBOY_TEST_SCOPE_KIND='rust_changed_union' \
    HOMEBOY_RUST_CHANGED_TEST_SELECTION_FILE="$WORKDIR/changed-selection.json" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)
if [[ "$OUTPUT" != *"Replaying Rust nextest shard: 2 runnable identities"* || "$OUTPUT" != *"inline_scope_runs"* || "$OUTPUT" != *"integration_scope_runs"* ]]; then
    printf 'Expected ARG_MAX-safe nextest batches for the exact union. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

# Renames and deletions can leave a candidate absent from the current inventory.
# They must widen safely instead of returning a green zero-test result.
cat > "$WORKDIR/changed-selection.json" <<'EOF'
{"schema":"homeboy/rust-changed-test-selection/v1","candidates":[{"package":"rust-changed-scope-smoke","target_kind":"test","target":"renamed_or_deleted","module":null}]}
EOF
OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_TEST_SCOPE_KIND='rust_changed_union' \
    HOMEBOY_RUST_CHANGED_TEST_SELECTION_FILE="$WORKDIR/changed-selection.json" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)
if [[ "$OUTPUT" != *"second_inline_scope_runs ... ok"* || "$OUTPUT" != *"integration_scope_runs ... ok"* ]]; then
    printf 'Expected unmatched changed selection to run the full suite. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_TEST_SCOPE_KIND='full' \
    HOMEBOY_TEST_SCOPE_MESSAGE='Changed files include multiple inline test modules; running full cargo test.' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
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

OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_TEST_SCOPE_KIND='full' \
    HOMEBOY_TEST_SCOPE_MESSAGE='Changed files include nested tests without a direct Cargo target; running full cargo test.' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)

if [[ "$OUTPUT" != *"Changed files include nested tests without a direct Cargo target; running full cargo test."* ]]; then
    printf 'Expected nested test fallback to full cargo test. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

if [[ "$OUTPUT" != *"2 passed"* || "$OUTPUT" != *"1 passed"* ]]; then
    printf 'Expected full fallback to run top-level and inline tests. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

WORKSPACE_DIR="$WORKDIR/workspace"
mkdir -p "$WORKSPACE_DIR/crates/targeted/src" "$WORKSPACE_DIR/crates/skipped/src"

cat > "$WORKSPACE_DIR/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/targeted", "crates/skipped"]
resolver = "2"
EOF

cat > "$WORKSPACE_DIR/crates/targeted/Cargo.toml" <<'EOF'
[package]
name = "targeted-pkg"
version = "0.1.0"
edition = "2021"
EOF

cat > "$WORKSPACE_DIR/crates/targeted/src/lib.rs" <<'EOF'
pub fn value() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn targeted_package_runs() {
        assert_eq!(super::value(), 1);
    }
}
EOF

cat > "$WORKSPACE_DIR/crates/skipped/Cargo.toml" <<'EOF'
[package]
name = "skipped-pkg"
version = "0.1.0"
edition = "2021"
EOF

cat > "$WORKSPACE_DIR/crates/skipped/src/lib.rs" <<'EOF'
#[cfg(test)]
mod tests {
    #[test]
    fn skipped_package_would_fail() {
        panic!("full workspace cargo test should not run this package");
    }
}
EOF

OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$WORKSPACE_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_TEST_SCOPE_KIND='args' \
    HOMEBOY_TEST_SCOPE_MESSAGE='Scoped to changed Cargo package: targeted-pkg.' \
    HOMEBOY_TEST_RUNNER_ARGS=$'-p\ntargeted-pkg' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)

if [[ "$OUTPUT" != *"Rust test scope: Scoped to changed Cargo package: targeted-pkg."* ]]; then
    printf 'Expected changed package scope. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

if [[ "$OUTPUT" != *"1 passed"* || "$OUTPUT" == *"full workspace cargo test should not run this package"* ]]; then
    printf 'Expected package-scoped cargo test to avoid the failing sibling package. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_TEST_SCOPE_KIND='full' \
    HOMEBOY_TEST_SCOPE_MESSAGE='Changed path is cross-cutting for Cargo: Cargo.toml' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)

if [[ "$OUTPUT" != *"Rust test scope: Changed path is cross-cutting for Cargo: Cargo.toml"* ]]; then
    printf 'Expected explicit Cargo.toml fallback. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi
