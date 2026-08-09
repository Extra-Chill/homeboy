#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
CORE_RUNTIME_DIR="${HOMEBOY_CORE_DIR}/crates/homeboy-extension/src/runtime"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-${CORE_RUNTIME_DIR}/failure-trap.sh}"
WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-${CORE_RUNTIME_DIR}/write-test-results.sh}"
SIDECAR_WRITER_HELPER="${HOMEBOY_RUNTIME_SIDECAR_WRITER:-${CORE_RUNTIME_DIR}/sidecar-writer.sh}"
RUNNER_PRELUDE_CORE_HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${CORE_RUNTIME_DIR}/runner-prelude.sh}"
RESOLVE_CONTEXT_CORE_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${CORE_RUNTIME_DIR}/resolve-context.sh}"
RUNNER_STEPS_CORE_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${CORE_RUNTIME_DIR}/runner-steps.sh}"
COMMAND_CAPTURE_CORE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-${CORE_RUNTIME_DIR}/command-capture.sh}"
PROJECT_SCRIPTS_HELPER="${ROOT_DIR}/scripts/lib/project-scripts.sh"
# Extension-owned shared libs are single-sourced in the top-level scripts/lib
# shared asset (materialized as an extensions/scripts/lib sibling at install).
FIX_RESULTS_HELPER="${ROOT_DIR}/scripts/lib/fix-results.sh"
BASH_PREFLIGHT_HELPER="${HOMEBOY_RUNTIME_BASH_PREFLIGHT:-${CORE_RUNTIME_DIR}/bash-preflight.sh}"
SETTINGS_HELPER="${ROOT_DIR}/scripts/lib/settings.sh"
RUNNER_HARNESS_HELPER="${ROOT_DIR}/scripts/lib/runner-harness.sh"
TEST_FAILURES_ADAPTER_HELPER="${ROOT_DIR}/scripts/lib/test-failures-adapter.sh"
LINT_FINDINGS_ADAPTER_HELPER="${ROOT_DIR}/scripts/lib/lint-findings-adapter.sh"

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

assert_sources_prelude() {
    local file="$1"
    if ! grep -Fq 'HOMEBOY_RUNTIME_RUNNER_PRELUDE' "$file" \
        && ! grep -Fq '/runner-harness.sh' "$file"; then
        echo "Expected $file to load the runner prelude directly (HOMEBOY_RUNTIME_RUNNER_PRELUDE) or via the shared runner-harness.sh" >&2
        exit 1
    fi
}

assert_sources_command_capture() {
    local file="$1"
    if ! grep -Fq 'HOMEBOY_RUNTIME_COMMAND_CAPTURE' "$file" \
        && ! grep -Fq 'homeboy_runner_harness_source_command_capture' "$file"; then
        echo "Expected $file to load command capture directly (HOMEBOY_RUNTIME_COMMAND_CAPTURE) or via the shared harness" >&2
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
node "$ROOT_DIR/tests/extension-shape-lint.mjs"
bash -c 'source "$1"; type homeboy_sidecar_emit >/dev/null; type homeboy_sidecar_write >/dev/null; type homeboy_sidecar_merge >/dev/null; type homeboy_merge_lint_findings >/dev/null; type homeboy_merge_test_failures >/dev/null; type homeboy_write_fix_results >/dev/null; type homeboy_merge_annotations >/dev/null' _ "$SIDECAR_WRITER_HELPER"
bash -c 'source "$1"; homeboy_require_bash_version 4' _ "$BASH_PREFLIGHT_HELPER"
bash -c 'source "$1"; type homeboy_project_init >/dev/null; type homeboy_project_has_script >/dev/null; type homeboy_project_run_script_command >/dev/null' _ "$PROJECT_SCRIPTS_HELPER"
bash -c 'source "$1"; type homeboy_runner_init >/dev/null; type homeboy_source_runtime_helper >/dev/null; type homeboy_require_bash_version >/dev/null' _ "$RUNNER_PRELUDE_CORE_HELPER"
assert_file "$FIX_RESULTS_HELPER"
bash -c 'source "$1"; type homeboy_fix_results_capture >/dev/null; type homeboy_fix_results_append_changed >/dev/null; type homeboy_fix_results_write >/dev/null' _ "$FIX_RESULTS_HELPER"
assert_file "$SETTINGS_HELPER"
bash -c 'source "$1"; type homeboy_setting >/dev/null; type homeboy_setting_bool >/dev/null; type homeboy_setting_array >/dev/null' _ "$SETTINGS_HELPER"
assert_file "$RUNNER_HARNESS_HELPER"
bash -c 'source "$1"; type homeboy_runner_harness_init >/dev/null; type homeboy_runner_harness_temp >/dev/null; type homeboy_runner_harness_source_command_capture >/dev/null; type homeboy_runner_harness_load_adapter >/dev/null; homeboy_runner_harness_load_adapter test-failures-adapter; type homeboy_test_failures_merge_file >/dev/null' _ "$RUNNER_HARNESS_HELPER"
assert_file "$TEST_FAILURES_ADAPTER_HELPER"
bash -c 'source "$1"; type homeboy_test_failures_merge_file >/dev/null; type homeboy_test_failure_record_json >/dev/null; type homeboy_test_failure_emit_record_json >/dev/null' _ "$TEST_FAILURES_ADAPTER_HELPER"
assert_file "$LINT_FINDINGS_ADAPTER_HELPER"
bash -c 'source "$1"; type homeboy_lint_findings_merge_file >/dev/null; type homeboy_lint_findings_write_empty >/dev/null; type homeboy_lint_findings_require_writer >/dev/null' _ "$LINT_FINDINGS_ADAPTER_HELPER"
bash -c 'source "$1"; type homeboy_run_step >/dev/null; type homeboy_run_step_capture >/dev/null; type homeboy_cleanup_step_capture >/dev/null' _ "$COMMAND_CAPTURE_CORE_HELPER"

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

HARNESS_TEMP=""
source "$RUNNER_HARNESS_HELPER"
homeboy_runner_harness_temp HARNESS_TEMP "homeboy-harness-smoke.XXXXXX"
if [ ! -f "$HARNESS_TEMP" ]; then
    echo "Expected harness temp helper to create a file" >&2
    exit 1
fi

HARNESS_EXIT_FILE="$TMP_DIR/harness-exit-file"
HARNESS_EXIT_DIR_PATH="$TMP_DIR/harness-exit-dir-path"
HARNESS_EXIT_SUMMARY="$TMP_DIR/harness-exit-summary"
printf 'temporary\n' > "$HARNESS_EXIT_FILE"
set +e
HOMEBOY_CACHE_DIR="$TMP_DIR" HOMEBOY_HARNESS_SUMMARY="$HARNESS_EXIT_SUMMARY" HOMEBOY_HARNESS_DIR_PATH="$HARNESS_EXIT_DIR_PATH" bash -c '
    source "$1"
    homeboy_print_failure_summary() { printf "failure summary\n" > "$HOMEBOY_HARNESS_SUMMARY"; }
    trap homeboy_print_failure_summary EXIT
    homeboy_runner_harness_register_cleanup "$3"
    homeboy_runner_harness_temp_dir OWNED_DIRECTORY
    printf "%s\n" "$OWNED_DIRECTORY" > "$HOMEBOY_HARNESS_DIR_PATH"
    exit 37
' _ "$RUNNER_HARNESS_HELPER" ignored "$HARNESS_EXIT_FILE"
HARNESS_EXIT=$?
set -e
HARNESS_EXIT_DIR="$(<"$HARNESS_EXIT_DIR_PATH")"
if [ "$HARNESS_EXIT" -ne 37 ] || [ -e "$HARNESS_EXIT_FILE" ] || [ -e "$HARNESS_EXIT_DIR" ]; then
    echo "Expected harness EXIT cleanup to preserve failure status and remove files and directories" >&2
    exit 1
fi
assert_contains "$HARNESS_EXIT_SUMMARY" 'failure summary'

HARNESS_SIDECAR_FILE="$TMP_DIR/harness-sidecar"
HARNESS_SIDECAR_TEMP="$TMP_DIR/harness-sidecar-temp"
printf 'temporary\n' > "$HARNESS_SIDECAR_TEMP"
set +e
HOMEBOY_HARNESS_SIDECAR="$HARNESS_SIDECAR_FILE" bash -c '
    source "$1"
    sidecar_exit_handler() { printf "sidecar\n" >> "$HOMEBOY_HARNESS_SIDECAR"; }
    trap sidecar_exit_handler EXIT
    homeboy_runner_harness_register_cleanup "$3"
    exit 23
' _ "$RUNNER_HARNESS_HELPER" ignored "$HARNESS_SIDECAR_TEMP"
HARNESS_SIDECAR_EXIT=$?
set -e
if [ "$HARNESS_SIDECAR_EXIT" -ne 23 ] || [ -e "$HARNESS_SIDECAR_TEMP" ] || [ "$(wc -l < "$HARNESS_SIDECAR_FILE" | tr -d ' ')" -ne 1 ]; then
    echo "Expected harness to compose the existing sidecar EXIT handler exactly once" >&2
    exit 1
fi

HARNESS_REPLACED_FILE="$TMP_DIR/harness-replaced-sidecars"
HARNESS_REPLACED_FIRST="$TMP_DIR/harness-replaced-first"
HARNESS_REPLACED_SECOND="$TMP_DIR/harness-replaced-second"
printf 'first\n' > "$HARNESS_REPLACED_FIRST"
printf 'second\n' > "$HARNESS_REPLACED_SECOND"
set +e
HOMEBOY_HARNESS_REPLACED="$HARNESS_REPLACED_FILE" bash -c '
    source "$1"
    first_exit_handler() { printf "first\n" >> "$HOMEBOY_HARNESS_REPLACED"; }
    replacement_exit_handler() { printf "replacement\n" >> "$HOMEBOY_HARNESS_REPLACED"; }
    trap first_exit_handler EXIT
    homeboy_runner_harness_register_cleanup "$2"
    trap replacement_exit_handler EXIT
    homeboy_runner_harness_register_cleanup "$3"
    exit 29
' _ "$RUNNER_HARNESS_HELPER" "$HARNESS_REPLACED_FIRST" "$HARNESS_REPLACED_SECOND"
HARNESS_REPLACED_EXIT=$?
set -e
if [ "$HARNESS_REPLACED_EXIT" -ne 29 ] || [ -e "$HARNESS_REPLACED_FIRST" ] || [ -e "$HARNESS_REPLACED_SECOND" ] || [ "$(wc -l < "$HARNESS_REPLACED_FILE" | tr -d ' ')" -ne 2 ]; then
    echo "Expected harness to compose an EXIT trap installed after initial cleanup registration" >&2
    exit 1
fi
assert_contains "$HARNESS_REPLACED_FILE" 'first'
assert_contains "$HARNESS_REPLACED_FILE" 'replacement'

HARNESS_DIR_ROOT="$TMP_DIR/harness-directory-root"
HARNESS_DIR_SYMLINK="$TMP_DIR/harness-directory-symlink"
mkdir -p "$HARNESS_DIR_ROOT"
HOMEBOY_CACHE_DIR="$HARNESS_DIR_ROOT" bash -c '
    source "$1"
    homeboy_runner_harness_temp_dir OWNED
    [ -d "$OWNED" ]
    ! homeboy_runner_harness_register_cleanup / directory
    ! homeboy_runner_harness_register_cleanup "$2" directory
    ln -s "$OWNED" "$3"
    ! homeboy_runner_harness_register_cleanup "$3" directory
    mkdir "$2/homeboy-runner.unexpected"
    ! homeboy_runner_harness_register_cleanup "$2/homeboy-runner.unexpected" directory
    homeboy_runner_harness_cleanup
    [ ! -e "$OWNED" ]
' _ "$RUNNER_HARNESS_HELPER" "$HARNESS_DIR_ROOT" "$HARNESS_DIR_SYMLINK"

HARNESS_REPLACEMENT_ROOT="$TMP_DIR/harness-replacement-root"
mkdir -p "$HARNESS_REPLACEMENT_ROOT"
HOMEBOY_CACHE_DIR="$HARNESS_REPLACEMENT_ROOT" bash -c '
    source "$1"
    homeboy_runner_harness_temp_dir MOVED
    mv "$MOVED" "$2/original"
    mkdir "$MOVED"
    ! homeboy_runner_harness_cleanup_path directory "$MOVED"
    [ -d "$MOVED" ] && [ -d "$2/original" ]
    homeboy_runner_harness_temp_dir LINKED
    mv "$LINKED" "$2/original-link"
    ln -s "$2" "$LINKED"
    ! homeboy_runner_harness_cleanup_path directory "$LINKED"
    [ -L "$LINKED" ] && [ -d "$2/original-link" ]
' _ "$RUNNER_HARNESS_HELPER" "$HARNESS_REPLACEMENT_ROOT"

GNU_STAT_BIN="$TMP_DIR/gnu-stat-bin"
GNU_STAT_ROOT="$TMP_DIR/gnu-stat-root"
mkdir -p "$GNU_STAT_BIN" "$GNU_STAT_ROOT"
cat > "$GNU_STAT_BIN/stat" <<'EOF'
#!/usr/bin/env bash
# GNU stat accepts -f but emits filesystem data, not device/inode identity.
printf 'filesystem-data\n'
EOF
chmod +x "$GNU_STAT_BIN/stat"
PATH="$GNU_STAT_BIN:$PATH" HOMEBOY_CACHE_DIR="$GNU_STAT_ROOT" bash -c '
    source "$1"
    homeboy_runner_harness_temp_dir OWNED
    homeboy_runner_harness_cleanup_path directory "$OWNED"
    [ ! -e "$OWNED" ]
' _ "$RUNNER_HARNESS_HELPER"

HARNESS_SERIALIZATION_ROOT="$TMP_DIR/harness-serialization-root"
HARNESS_UNRELATED_FILE="$TMP_DIR/harness-unrelated-file"
mkdir -p "$HARNESS_SERIALIZATION_ROOT"
printf 'retain\n' > "$HARNESS_UNRELATED_FILE"
HOMEBOY_CACHE_DIR="$HARNESS_SERIALIZATION_ROOT" bash -c '
    source "$1"
    unsafe_path="$2/unsafe"$'"'"'\tfile\nfile'"'"'
    : > "$unsafe_path"
    ! homeboy_runner_harness_register_cleanup "$unsafe_path"
    ! homeboy_runner_harness_temp BAD_TEMPLATE $'"'"'homeboy-runner.\tXXXXXX'"'"'
    homeboy_runner_harness_temp SPACE_TEMPLATE "homeboy runner.XXXXXX"
    [ -f "$SPACE_TEMPLATE" ]
    homeboy_runner_harness_cleanup
    [ ! -e "$SPACE_TEMPLATE" ]
    [ -e "$unsafe_path" ]
' _ "$RUNNER_HARNESS_HELPER" "$HARNESS_SERIALIZATION_ROOT"
[ -e "$HARNESS_UNRELATED_FILE" ] || { echo "Unexpected cleanup removed unrelated file" >&2; exit 1; }

python3 - "$ROOT_DIR" <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
violations = []
for path in root.rglob("*.sh"):
    if "tests" in path.parts or ".git" in path.parts:
        continue
    if path == root / "scripts/lib/runner-harness.sh":
        continue
    lines = path.read_text(encoding="utf-8").splitlines()
    first_registration = next((index for index, line in enumerate(lines) if re.search(r"homeboy_runner_harness_(temp|temp_dir|register_cleanup)\b", line)), None)
    if first_registration is None:
        continue
    if any(re.search(r"\btrap\b.*\bEXIT\b", line) for line in lines[first_registration + 1:]):
        violations.append(str(path.relative_to(root)))
assert not violations, f"EXIT trap installed after harness cleanup registration: {violations}"
PY
homeboy_runner_harness_cleanup
if [ -e "$HARNESS_TEMP" ]; then
    echo "Expected harness cleanup to remove temp file" >&2
    exit 1
fi

FAILURE_RECORD="$(source "$TEST_FAILURES_ADAPTER_HELPER"; homeboy_test_failure_record_json smoke 'suite::test' suite tests/smoke.test 12 'failed hard' assertion 'stdout tail' '')"
printf '%s' "$FAILURE_RECORD" | node -e 'const fs=require("node:fs"); const record=JSON.parse(fs.readFileSync(0,"utf8")); if (record.test_id!=="suite::test" || record.file!=="tests/smoke.test" || record.line!==12 || !/^[a-f0-9]{64}$/.test(record.fingerprint)) process.exit(1);'

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
    assert_sources_prelude "$ROOT_DIR/$runner"
done

for runner in \
    nodejs/scripts/bench/bench-runner.sh \
    nodejs/scripts/fuzz/fuzz-runner.sh \
    nodejs/scripts/trace/trace-runner.sh \
    rust/scripts/bench/bench-runner.sh \
    wordpress/scripts/bench/bench-runner.sh; do
    assert_sources "$ROOT_DIR/$runner" 'HOMEBOY_RUNTIME_BASH_PREFLIGHT'
done

for runner in \
    nodejs/scripts/bench/bench-runner.sh \
    nodejs/scripts/format.sh \
    nodejs/scripts/fuzz/fuzz-runner.sh \
    nodejs/scripts/trace/trace-runner.sh \
    rust/scripts/bench/bench-runner.sh \
    rust/scripts/format.sh \
    swift/scripts/lint-runner.sh \
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
    assert_sources_command_capture "$ROOT_DIR/$runner"
done

for runner in \
    nodejs/scripts/fuzz/fuzz-runner.sh \
    nodejs/scripts/test/test-runner.sh \
    rust/scripts/bench/bench-runner.sh \
    rust/scripts/test-runner.sh \
    wordpress/scripts/test/test-runner.sh; do
    assert_sources "$ROOT_DIR/$runner" '/settings.sh'
done

for runner in \
    nodejs/scripts/lint/lint-runner.sh \
    rust/scripts/lint-runner.sh \
    wordpress/scripts/lint/lint-runner.sh; do
    assert_sources "$ROOT_DIR/$runner" '/fix-results.sh'
done

echo "runtime helper smoke passed"
