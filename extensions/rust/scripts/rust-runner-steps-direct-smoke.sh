#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROJECT_DIR="$(mktemp -d)"
trap 'rm -rf "$PROJECT_DIR"' EXIT
RUNNER_PRELUDE_HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-$PROJECT_DIR/runner-prelude.sh}"
COMMAND_CAPTURE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-$PROJECT_DIR/command-capture.sh}"

cat > "$PROJECT_DIR/runner-prelude.sh" <<'EOF'
homeboy_runner_init() {
    PROJECT_PATH="$HOMEBOY_COMPONENT_PATH"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
should_run_step() { [ "${HOMEBOY_STEP:-}" != "none" ]; }
EOF
cat > "$PROJECT_DIR/command-capture.sh" <<'EOF'
homeboy_run_step_capture() { local output_var="$1" exit_var="$2"; shift 3; [ "$1" = -- ] && shift; local output status=0; output="$(mktemp)"; "$@" >"$output" 2>&1 || status=$?; printf -v "$output_var" '%s' "$output"; printf -v "$exit_var" '%s' "$status"; return "$status"; }
homeboy_cleanup_step_capture() { rm -f "$1"; }
EOF

cat > "$PROJECT_DIR/Cargo.toml" <<'EOF'
[package]
name = "runner_steps_direct_smoke"
version = "0.1.0"
edition = "2021"
EOF

OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$ROOT_DIR/rust" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_STEP="none" \
    env -u HOMEBOY_RUNTIME_RUNNER_STEPS \
    bash "$ROOT_DIR/rust/scripts/lint-runner.sh"
)

if [[ "$OUTPUT" != *"Skipping cargo fmt (step filter)"* ]]; then
    printf 'Expected direct Rust lint invocation to load runner-step fallback and skip fmt. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

if [[ "$OUTPUT" != *"Skipping cargo clippy (step filter)"* ]]; then
    printf 'Expected direct Rust lint invocation to load runner-step fallback and skip clippy. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$ROOT_DIR/rust" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_STEP="none" \
    env -u HOMEBOY_RUNTIME_RUNNER_STEPS \
    bash "$ROOT_DIR/rust/scripts/test-runner.sh"
)

if [[ "$OUTPUT" != *"Skipping lint (step filter)"* ]]; then
    printf 'Expected direct Rust test invocation to load runner-step fallback and skip lint. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

if [[ "$OUTPUT" != *"Skipping tests (step filter)"* ]]; then
    printf 'Expected direct Rust test invocation to load runner-step fallback and skip tests. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

echo "rust runner-steps direct smoke ok"
