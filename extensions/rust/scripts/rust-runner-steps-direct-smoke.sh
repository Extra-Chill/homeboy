#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
RUNNER_PRELUDE_HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/runner-prelude.sh}"
COMMAND_CAPTURE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/command-capture.sh}"
PROJECT_DIR="$(mktemp -d)"
trap 'rm -rf "$PROJECT_DIR"' EXIT

if [ ! -f "$RUNNER_PRELUDE_HELPER" ]; then
    echo "Missing runner prelude helper: $RUNNER_PRELUDE_HELPER" >&2
    exit 1
fi
if [ ! -f "$COMMAND_CAPTURE_HELPER" ]; then
    echo "Missing command capture helper: $COMMAND_CAPTURE_HELPER" >&2
    exit 1
fi

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
