#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/failure-trap.sh}"
WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/write-test-results.sh}"
SIDECAR_WRITER_HELPER="${HOMEBOY_RUNTIME_SIDECAR_WRITER:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/sidecar-writer.sh}"
RUNNER_PRELUDE_CORE_HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/runner-prelude.sh}"
RESOLVE_CONTEXT_CORE_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/resolve-context.sh}"
RUNNER_STEPS_CORE_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/runner-steps.sh}"
COMMAND_CAPTURE_CORE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/command-capture.sh}"
PROJECT_SCRIPTS_HELPER="${ROOT_DIR}/scripts/lib/project-scripts.sh"
# Extension-owned shared libs are single-sourced in the top-level scripts/lib
# shared asset (materialized as an extensions/scripts/lib sibling at install).
FIX_RESULTS_HELPER="${ROOT_DIR}/scripts/lib/fix-results.sh"
BASH_PREFLIGHT_HELPER="${HOMEBOY_RUNTIME_BASH_PREFLIGHT:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/bash-preflight.sh}"
SETTINGS_HELPER="${ROOT_DIR}/scripts/lib/settings.sh"

assert_file() {
    local path="$1"
    if [ ! -f "$path" ]; then
        echo "Missing required file: $path" >&2
        exit 1
    fi
}

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        echo "Actual contents:" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq "$unexpected" "$file"; then
        echo "Expected $file not to contain: $unexpected" >&2
        echo "Actual contents:" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_sources() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to source shared helper via: $expected" >&2
        exit 1
    fi
}

assert_file "$FAILURE_TRAP_HELPER"
assert_file "$WRITE_TEST_RESULTS_HELPER"
assert_file "$SIDECAR_WRITER_HELPER"
assert_file "$RUNNER_PRELUDE_CORE_HELPER"
assert_file "$RESOLVE_CONTEXT_CORE_HELPER"
assert_file "$RUNNER_STEPS_CORE_HELPER"
assert_file "$COMMAND_CAPTURE_CORE_HELPER"
assert_file "$BASH_PREFLIGHT_HELPER"
assert_file "$PROJECT_SCRIPTS_HELPER"
bash -c 'source "$1"; type homeboy_sidecar_emit >/dev/null; type homeboy_sidecar_write >/dev/null; type homeboy_sidecar_merge >/dev/null; type homeboy_merge_lint_findings >/dev/null; type homeboy_merge_test_failures >/dev/null; type homeboy_write_fix_results >/dev/null; type homeboy_merge_annotations >/dev/null' _ "$SIDECAR_WRITER_HELPER"
bash -c 'source "$1"; homeboy_require_bash_version 4' _ "$BASH_PREFLIGHT_HELPER"
bash -c 'source "$1"; type homeboy_project_init >/dev/null; type homeboy_project_has_script >/dev/null; type homeboy_project_run_script_command >/dev/null' _ "$PROJECT_SCRIPTS_HELPER"
bash -c 'source "$1"; type homeboy_runner_init >/dev/null; type homeboy_source_runtime_helper >/dev/null; type homeboy_require_bash_version >/dev/null' _ "$RUNNER_PRELUDE_CORE_HELPER"
assert_file "$FIX_RESULTS_HELPER"
bash -c 'source "$1"; type homeboy_fix_results_capture >/dev/null; type homeboy_fix_results_append_changed >/dev/null; type homeboy_fix_results_write >/dev/null' _ "$FIX_RESULTS_HELPER"
assert_file "$SETTINGS_HELPER"
bash -c 'source "$1"; type homeboy_setting >/dev/null; type homeboy_setting_bool >/dev/null; type homeboy_setting_array >/dev/null' _ "$SETTINGS_HELPER"
bash -c 'source "$1"; type homeboy_run_step >/dev/null; type homeboy_run_step_capture >/dev/null; type homeboy_cleanup_step_capture >/dev/null' _ "$COMMAND_CAPTURE_CORE_HELPER"

# Single-source guarantee: extension-owned shared libs (settings, fix-results,
# project-scripts) live only in the top-level scripts/lib shared asset and are
# never re-vendored into a per-language lib dir.
for vendored in \
    nodejs/scripts/lib/settings.sh \
    rust/scripts/lib/settings.sh \
    wordpress/scripts/lib/settings.sh \
    nodejs/scripts/lib/fix-results.sh \
    rust/scripts/lib/fix-results.sh \
    wordpress/scripts/lib/fix-results.sh \
    nodejs/scripts/lib/project-scripts.sh; do
    if [ -e "${ROOT_DIR}/${vendored}" ]; then
        echo "Shared lib must be single-sourced in scripts/lib; found vendored copy: ${vendored}" >&2
        exit 1
    fi
done

# Core-owned runtime helpers (runner-prelude, runner-steps, command-capture,
# resolve-context, sidecar-writer) are materialized by homeboy core and resolved
# by every runner via the required HOMEBOY_RUNTIME_* env vars. They must never be
# re-vendored as per-language scripts/lib shims.
for core_owned in \
    nodejs/scripts/lib/runner-prelude.sh \
    rust/scripts/lib/runner-prelude.sh \
    wordpress/scripts/lib/runner-prelude.sh \
    nodejs/scripts/lib/resolve-context.sh \
    rust/scripts/lib/resolve-context.sh \
    wordpress/scripts/lib/resolve-context.sh \
    swift/scripts/lib/resolve-context.sh \
    nodejs/scripts/lib/command-capture.sh \
    rust/scripts/lib/command-capture.sh \
    wordpress/scripts/lib/command-capture.sh \
    wordpress/scripts/lib/runner-steps.sh \
    wordpress/scripts/lib/sidecar-writer.sh; do
    if [ -e "${ROOT_DIR}/${core_owned}" ]; then
        echo "Core-owned runtime helper must resolve from homeboy core, not a vendored shim: ${core_owned}" >&2
        exit 1
    fi
done

# Resolution wiring: each runner's relative fallback path must resolve to the
# canonical shared lib. The source-repo layout (scripts/lib as a sibling of each
# language tree) matches the installed layout (extensions/scripts/lib sibling of
# extensions/<id>), so the same relative depth resolves in both. This catches a
# wrong ../ depth before it ships.
assert_resolves() {
    local from_dir="$1" rel="$2" canonical="$3" resolved
    if ! resolved="$(cd "${ROOT_DIR}/${from_dir}" && cd "$(dirname "$rel")" 2>/dev/null && pwd)/$(basename "$rel")"; then
        echo "Could not resolve $rel from $from_dir" >&2
        exit 1
    fi
    if ! cmp -s "$resolved" "$canonical"; then
        echo "Runner in $from_dir resolves $rel to $resolved, expected canonical $canonical" >&2
        exit 1
    fi
}
assert_resolves nodejs/scripts/test    ../../../scripts/lib/settings.sh         "$SETTINGS_HELPER"
assert_resolves rust/scripts           ../../scripts/lib/settings.sh            "$SETTINGS_HELPER"
assert_resolves rust/scripts/bench     ../../../scripts/lib/settings.sh         "$SETTINGS_HELPER"
assert_resolves wordpress/scripts/test ../../../scripts/lib/settings.sh         "$SETTINGS_HELPER"
assert_resolves wordpress/scripts/lib  ../../../scripts/lib/settings.sh         "$SETTINGS_HELPER"
assert_resolves nodejs/scripts/lint    ../../../scripts/lib/fix-results.sh      "$FIX_RESULTS_HELPER"
assert_resolves rust/scripts           ../../scripts/lib/fix-results.sh         "$FIX_RESULTS_HELPER"
assert_resolves wordpress/scripts/lint ../../../scripts/lib/fix-results.sh      "$FIX_RESULTS_HELPER"
assert_resolves nodejs/scripts/lib     ../../../scripts/lib/project-scripts.sh  "$PROJECT_SCRIPTS_HELPER"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

ANNOTATIONS_DIR="$TMP_DIR/annotations"
ANNOTATIONS_SOURCE="$TMP_DIR/extra-annotations.json"
printf '[{"file":"b.php","line":2}]\n' > "$ANNOTATIONS_SOURCE"
HOMEBOY_ANNOTATIONS_DIR="$ANNOTATIONS_DIR" bash -c 'source "$1"; homeboy_sidecar_emit annotation.phpcs "{\"file\":\"a.php\",\"line\":1}"; homeboy_merge_annotations phpstan "$2"' _ "$SIDECAR_WRITER_HELPER" "$ANNOTATIONS_SOURCE"
assert_contains "$ANNOTATIONS_DIR/phpcs.json" '"file":"a.php"'
assert_contains "$ANNOTATIONS_DIR/phpstan.json" '"file":"b.php"'

# shellcheck source=/dev/null
source "$COMMAND_CAPTURE_CORE_HELPER"
FAILED_STEP=""
FAILURE_OUTPUT=""
SUCCESS_OUTPUT_FILE=""
SUCCESS_EXIT=""
homeboy_run_step_capture SUCCESS_OUTPUT_FILE SUCCESS_EXIT "successful command" -- bash -c 'printf "ok\n"' || true
if [ "$SUCCESS_EXIT" -ne 0 ]; then
    echo "Expected successful command capture to preserve exit 0" >&2
    exit 1
fi
assert_contains "$SUCCESS_OUTPUT_FILE" "ok"
homeboy_cleanup_step_capture "$SUCCESS_OUTPUT_FILE"
if [ -e "$SUCCESS_OUTPUT_FILE" ]; then
    echo "Expected command capture cleanup to remove output file" >&2
    exit 1
fi

FAILED_OUTPUT_FILE=""
FAILED_EXIT=""
homeboy_run_step_capture FAILED_OUTPUT_FILE FAILED_EXIT "failing command" -- bash -c 'printf "first\n"; printf "last\n"; exit 42' || true
if [ "$FAILED_EXIT" -ne 42 ]; then
    echo "Expected failing command capture to preserve exit 42, got ${FAILED_EXIT}" >&2
    exit 1
fi
if [ "$FAILED_STEP" != "failing command" ]; then
    echo "Expected failing command capture to set FAILED_STEP" >&2
    exit 1
fi
case "$FAILURE_OUTPUT" in
    *last*) ;;
    *)
        echo "Expected failing command capture to set FAILURE_OUTPUT tail" >&2
        exit 1
        ;;
esac
homeboy_cleanup_step_capture "$FAILED_OUTPUT_FILE"

FAILED_STEP=""
FAILURE_OUTPUT=""
if homeboy_run_step "wrapped command" -- bash -c 'printf "wrapped\n"; exit 7'; then
    echo "Expected wrapped command capture to preserve failure" >&2
    exit 1
fi
if [ "$FAILED_STEP" != "wrapped command" ]; then
    echo "Expected wrapped command capture to set FAILED_STEP" >&2
    exit 1
fi

SIDECAR_TMP_DIR="$TMP_DIR/sidecars"
mkdir -p "$SIDECAR_TMP_DIR/annotations"
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_LINT_FINDINGS_FILE="$SIDECAR_TMP_DIR/lint.json" \
HOMEBOY_TEST_FAILURES_FILE="$SIDECAR_TMP_DIR/test.json" \
HOMEBOY_FIX_RESULTS_FILE="$SIDECAR_TMP_DIR/fix.json" \
HOMEBOY_ANNOTATIONS_DIR="$SIDECAR_TMP_DIR/annotations" \
    bash -c 'source "$HOMEBOY_RUNTIME_SIDECAR_WRITER"; homeboy_sidecar_emit lint.finding "{\"id\":\"lint\"}"; homeboy_sidecar_write test.failures "{\"test_id\":\"test\"}"; homeboy_sidecar_merge fix.results <(printf "[{\"file\":\"fixed.php\"}]\n"); homeboy_sidecar_emit annotation.phpcs "{\"file\":\"plugin.php\",\"line\":1}"'
assert_contains "$SIDECAR_TMP_DIR/lint.json" '"id":"lint"'
assert_contains "$SIDECAR_TMP_DIR/test.json" '"test_id":"test"'
assert_contains "$SIDECAR_TMP_DIR/fix.json" '"file":"fixed.php"'
assert_contains "$SIDECAR_TMP_DIR/annotations/phpcs.json" '"file":"plugin.php"'

WORDPRESS_OUTPUT="$TMP_DIR/phpunit.txt"
WORDPRESS_RESULTS="$TMP_DIR/wordpress-results.json"
cat > "$WORDPRESS_OUTPUT" <<'EOF'
Tests: 12, Assertions: 30, Errors: 1, Failures: 2, Warnings: 1, Skipped: 3, Incomplete: 1, Risky: 1.
EOF

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_TEST_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$WORDPRESS_RESULTS" \
    bash "$ROOT_DIR/wordpress/scripts/test/parse-test-results.sh" "$WORDPRESS_OUTPUT" >/dev/null
assert_contains "$WORDPRESS_RESULTS" '"total": 12'
assert_contains "$WORDPRESS_RESULTS" '"passed": 3'
assert_contains "$WORDPRESS_RESULTS" '"failed": 3'
assert_contains "$WORDPRESS_RESULTS" '"skipped": 6'
assert_not_contains "$WORDPRESS_RESULTS" '"partial"'

WORDPRESS_PARTIAL_OUTPUT="$TMP_DIR/phpunit-partial.txt"
WORDPRESS_PARTIAL_RESULTS="$TMP_DIR/wordpress-partial-results.json"
cat > "$WORDPRESS_PARTIAL_OUTPUT" <<'EOF'
 ✔ First test
 ✔ Second test
 ✘ Third test
EOF

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_TEST_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$WORDPRESS_PARTIAL_RESULTS" \
    bash "$ROOT_DIR/wordpress/scripts/test/parse-test-results.sh" "$WORDPRESS_PARTIAL_OUTPUT" >/dev/null
assert_contains "$WORDPRESS_PARTIAL_RESULTS" '"total": 3'
assert_contains "$WORDPRESS_PARTIAL_RESULTS" '"passed": 2'
assert_contains "$WORDPRESS_PARTIAL_RESULTS" '"failed": 1'
assert_contains "$WORDPRESS_PARTIAL_RESULTS" '"partial": "testdox-fallback"'

WORDPRESS_HOST_SMOKE_OUTPUT="$TMP_DIR/host-smoke.txt"
WORDPRESS_HOST_SMOKE_RESULTS="$TMP_DIR/host-smoke-results.json"
cat > "$WORDPRESS_HOST_SMOKE_OUTPUT" <<'EOF'
HOST_SMOKE_BEGIN:tests/wiki/installed-brain-discovery-smoke.php
[FAIL] packaged meetups brain surfaces in brains registry
HOST_SMOKE_FAIL:tests/wiki/installed-brain-discovery-smoke.php:exit=1

Host smoke test failed: tests/wiki/installed-brain-discovery-smoke.php
EOF

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_TEST_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$WORDPRESS_HOST_SMOKE_RESULTS" \
    bash "$ROOT_DIR/wordpress/scripts/test/parse-test-results.sh" "$WORDPRESS_HOST_SMOKE_OUTPUT" >/dev/null
assert_contains "$WORDPRESS_HOST_SMOKE_RESULTS" '"total": 1'
assert_contains "$WORDPRESS_HOST_SMOKE_RESULTS" '"passed": 0'
assert_contains "$WORDPRESS_HOST_SMOKE_RESULTS" '"failed": 1'
assert_contains "$WORDPRESS_HOST_SMOKE_RESULTS" '"partial": "host-smoke-failure"'

RUST_OUTPUT="$TMP_DIR/cargo-test.txt"
RUST_RESULTS="$TMP_DIR/rust-results.json"
cat > "$RUST_OUTPUT" <<'EOF'
test result: ok. 10 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out;
test result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;
EOF

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_TEST_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$RUST_RESULTS" \
    bash "$ROOT_DIR/rust/scripts/parse-test-results.sh" "$RUST_OUTPUT" >/dev/null
assert_contains "$RUST_RESULTS" '"total": 17'
assert_contains "$RUST_RESULTS" '"passed": 14'
assert_contains "$RUST_RESULTS" '"failed": 1'
assert_contains "$RUST_RESULTS" '"skipped": 2'

RUST_EMPTY_OUTPUT="$TMP_DIR/cargo-test-empty.txt"
RUST_EMPTY_RESULTS="$TMP_DIR/rust-empty-results.json"
printf 'compiler output without a cargo summary\n' > "$RUST_EMPTY_OUTPUT"
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_TEST_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$RUST_EMPTY_RESULTS" \
    bash "$ROOT_DIR/rust/scripts/parse-test-results.sh" "$RUST_EMPTY_OUTPUT" >/dev/null
if [ -e "$RUST_EMPTY_RESULTS" ]; then
    echo "Rust parser should not write a sidecar when no test summary exists" >&2
    exit 1
fi

NODE_PROJECT="$TMP_DIR/node-project"
mkdir -p "$NODE_PROJECT"
cat > "$NODE_PROJECT/package.json" <<'EOF'
{"name":"runtime-helper-smoke","scripts":{}}
EOF

PROJECT_HELPER_NODE_PROJECT="$TMP_DIR/project-helper-node"
mkdir -p "$PROJECT_HELPER_NODE_PROJECT/subdir"
cat > "$PROJECT_HELPER_NODE_PROJECT/package.json" <<'EOF'
{"name":"project-helper-node","scripts":{"test":"node --test","lint:fix":"eslint . --fix"}}
EOF
bash -c '
    source "$1"
    PROJECT_PATH="$2/subdir"
    homeboy_project_init --ecosystem node --path "$PROJECT_PATH"
    [ "$HOMEBOY_PROJECT_ROOT" = "$2" ]
    [ "$(homeboy_project_run_script_command test)" = "npm run test" ]
    homeboy_project_has_script test
    homeboy_project_has_script lint:fix
    ! homeboy_project_has_script build
' _ "$PROJECT_SCRIPTS_HELPER" "$PROJECT_HELPER_NODE_PROJECT"

PROJECT_HELPER_PNPM_PROJECT="$TMP_DIR/project-helper-pnpm"
mkdir -p "$PROJECT_HELPER_PNPM_PROJECT"
cat > "$PROJECT_HELPER_PNPM_PROJECT/package.json" <<'EOF'
{"name":"project-helper-pnpm","scripts":{"test":"node --test"}}
EOF
touch "$PROJECT_HELPER_PNPM_PROJECT/pnpm-lock.yaml"
bash -c '
    source "$1"
    homeboy_project_init --ecosystem node --path "$2"
    [ "$HOMEBOY_PROJECT_PACKAGE_MANAGER" = "pnpm" ]
    [ "$(homeboy_project_run_script_command test)" = "pnpm run test" ]
' _ "$PROJECT_SCRIPTS_HELPER" "$PROJECT_HELPER_PNPM_PROJECT"

set +e
HOMEBOY_RUNTIME_FAILURE_TRAP="$FAILURE_TRAP_HELPER" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_CORE_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_CORE_HELPER" \
HOMEBOY_EXTENSION_PATH="$ROOT_DIR/nodejs" \
HOMEBOY_COMPONENT_PATH="$NODE_PROJECT" \
HOMEBOY_COMPONENT_ID="runtime-helper-smoke" \
    bash "$ROOT_DIR/nodejs/scripts/build/build-runner.sh" >"$TMP_DIR/node-build.out" 2>&1
NODE_EXIT=$?
set -e
if [ "$NODE_EXIT" -eq 0 ]; then
    echo "Expected node build runner to fail without scripts.build" >&2
    exit 1
fi
assert_contains "$TMP_DIR/node-build.out" 'BUILD FAILED: No build defined'
assert_contains "$TMP_DIR/node-build.out" 'Error details:'

for runner in \
    nodejs/scripts/build/build-runner.sh \
    nodejs/scripts/lint/lint-runner.sh \
    nodejs/scripts/test/test-runner.sh \
    rust/scripts/lint-runner.sh \
    rust/scripts/test-runner.sh \
    swift/scripts/test-runner.sh \
    wordpress/scripts/lint/lint-runner.sh \
    wordpress/scripts/test/test-runner.sh; do
    assert_sources "$ROOT_DIR/$runner" 'HOMEBOY_RUNTIME_RUNNER_PRELUDE'
done

for runner in \
    nodejs/scripts/bench/bench-runner.sh \
    nodejs/scripts/trace/trace-runner.sh \
    rust/scripts/bench/bench-runner.sh \
    wordpress/scripts/bench/bench-runner.sh; do
    assert_sources "$ROOT_DIR/$runner" 'HOMEBOY_RUNTIME_BASH_PREFLIGHT'
done

for runner in \
    nodejs/scripts/bench/bench-runner.sh \
    nodejs/scripts/format.sh \
    nodejs/scripts/trace/trace-runner.sh \
    nodejs/scripts/validate.sh \
    rust/scripts/bench/bench-runner.sh \
    rust/scripts/format.sh \
    rust/scripts/validate.sh \
    swift/scripts/lint-runner.sh \
    swift/scripts/validate.sh \
    wordpress/scripts/bench/bench-runner-wp-codebox.sh \
    wordpress/scripts/build/build.sh \
    wordpress/scripts/lint/eslint-runner.sh \
    wordpress/scripts/lint/lint-runner-core-dev.sh \
    wordpress/scripts/test/test-runner-host-smoke-wp.sh \
    wordpress/scripts/test/test-runner-wp-codebox.sh; do
    assert_sources "$ROOT_DIR/$runner" 'HOMEBOY_RUNTIME_RESOLVE_CONTEXT'
done

for runner in \
    nodejs/scripts/build/build-runner.sh \
    nodejs/scripts/test/test-runner.sh \
    rust/scripts/lint-runner.sh \
    rust/scripts/test-runner.sh; do
    assert_sources "$ROOT_DIR/$runner" 'HOMEBOY_RUNTIME_COMMAND_CAPTURE'
done

for runner in \
    nodejs/scripts/test/test-runner.sh \
    rust/scripts/bench/bench-runner.sh \
    rust/scripts/test-runner.sh \
    wordpress/scripts/test/test-runner.sh; do
    assert_sources "$ROOT_DIR/$runner" 'scripts/lib/settings.sh'
done

for runner in \
    nodejs/scripts/lint/lint-runner.sh \
    rust/scripts/lint-runner.sh \
    wordpress/scripts/lint/lint-runner.sh; do
    assert_sources "$ROOT_DIR/$runner" 'scripts/lib/fix-results.sh'
done

echo "runtime helper smoke passed"
