#!/usr/bin/env bash
set -euo pipefail

# A derived test scope that selects zero tests must widen to the full command
# rather than reporting a failed build. The filter is built from changed file
# paths, so a module mounted with `#[path]` compiles a filter matching nothing
# and the run would otherwise fail for code that was never executed.
#
# Guards all four outcomes: widen-and-pass, no needless widening, real failures
# still failing, and a genuinely empty suite still failing closed.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORK_DIR="$(mktemp -d -t homeboy-rust-zero-scope.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT

cat > "${WORK_DIR}/runner-prelude.sh" <<'EOF'
homeboy_runner_init() {
    PROJECT_PATH="$HOMEBOY_COMPONENT_PATH"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
should_run_step() { return 0; }
EOF
cat > "${WORK_DIR}/command-capture.sh" <<'EOF'
homeboy_run_step_capture() { local output_var="$1" exit_var="$2"; shift 3; [ "$1" = -- ] && shift; local output status=0; output="$(mktemp)"; "$@" 2>&1 | tee "$output"; status=${PIPESTATUS[0]}; printf -v "$output_var" '%s' "$output"; printf -v "$exit_var" '%s' "$status"; return "$status"; }
homeboy_cleanup_step_capture() { rm -f "$1"; }
EOF

if ! command -v cargo >/dev/null 2>&1; then
    printf 'SKIP: cargo is not installed\n'
    exit 0
fi

# Mirrors the real breakage: `lib_tests.rs` is mounted by `lib.rs` as `tests`,
# so a filter derived from the file name matches nothing.
make_project() {
    local dir="$1" test_body="$2"
    mkdir -p "${dir}/src"
    cat > "${dir}/Cargo.toml" <<TOML
[package]
name = "$(basename "${dir}")"
version = "0.1.0"
edition = "2021"

[workspace]
TOML
    cat > "${dir}/src/lib.rs" <<'RS'
pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
RS
    printf '%s' "${test_body}" > "${dir}/src/lib_tests.rs"
}

run_runner() {
    local project="$1" filter="$2" scope_kind="${3:-rust_filter}"
    set +e
    HOMEBOY_EXTENSION_PATH="${EXTENSION_DIR}" \
        HOMEBOY_COMPONENT_PATH="${project}" \
        HOMEBOY_SKIP_LINT=1 \
        HOMEBOY_CHANGED_TEST_FILES="src/lib_tests.rs" \
        HOMEBOY_TEST_SCOPE_KIND="${scope_kind}" \
        HOMEBOY_TEST_RUNNER_ARGS="$(printf -- '--\n%s' "${filter}")" \
        HOMEBOY_RUNTIME_RUNNER_PRELUDE="${WORK_DIR}/runner-prelude.sh" \
        HOMEBOY_RUNTIME_COMMAND_CAPTURE="${WORK_DIR}/command-capture.sh" \
        bash "${EXTENSION_DIR}/scripts/test-runner.sh" >"${WORK_DIR}/out.log" 2>&1
    RUNNER_EXIT=$?
    set -e
    RUNNER_OUTPUT="$(cat "${WORK_DIR}/out.log")"
}

passing_test='use super::add;

#[test]
fn adds() { assert_eq!(add(1, 2), 3); }
'
failing_test='use super::add;

#[test]
fn adds() { assert_eq!(add(1, 2), 99); }
'

# 1. Filter matches nothing -> widen to the full suite and pass.
make_project "${WORK_DIR}/widen" "${passing_test}"
run_runner "${WORK_DIR}/widen" 'lib_tests'
if [ "${RUNNER_EXIT}" -ne 0 ]; then
    printf 'FAIL: zero-match scope did not widen to the full suite (exit %s)\n' "${RUNNER_EXIT}"
    printf '%s\n' "${RUNNER_OUTPUT}"
    exit 1
fi
if ! printf '%s' "${RUNNER_OUTPUT}" | grep -q 'Derived test scope executed no tests'; then
    printf 'FAIL: zero-match scope widened without reporting why\n'
    exit 1
fi
if ! printf '%s' "${RUNNER_OUTPUT}" | grep -q 'test tests::adds ... ok'; then
    printf 'FAIL: widened run did not execute the mounted test\n'
    exit 1
fi
printf 'PASS: zero-match derived scope widens to the full suite and passes\n'

# 2. Filter matches tests -> must not widen (no wasted full run).
make_project "${WORK_DIR}/scoped" "${passing_test}"
run_runner "${WORK_DIR}/scoped" 'tests::adds'
if [ "${RUNNER_EXIT}" -ne 0 ]; then
    printf 'FAIL: matching scope should pass (exit %s)\n' "${RUNNER_EXIT}"
    exit 1
fi
if printf '%s' "${RUNNER_OUTPUT}" | grep -q 'Derived test scope executed no tests'; then
    printf 'FAIL: matching scope widened to the full suite unnecessarily\n'
    exit 1
fi
printf 'PASS: a scope that selects tests does not widen\n'

# 3. Widening must not mask genuine test failures.
make_project "${WORK_DIR}/failing" "${failing_test}"
run_runner "${WORK_DIR}/failing" 'lib_tests'
if [ "${RUNNER_EXIT}" -eq 0 ]; then
    printf 'FAIL: widened run reported success despite a failing test\n'
    exit 1
fi
printf 'PASS: widened run still surfaces real test failures\n'

# 4. A suite that genuinely runs nothing must still fail closed.
make_project "${WORK_DIR}/empty" 'pub fn helper() {}
'
run_runner "${WORK_DIR}/empty" 'lib_tests'
if [ "${RUNNER_EXIT}" -eq 0 ]; then
    printf 'FAIL: empty suite should fail closed after widening\n'
    exit 1
fi
if ! printf '%s' "${RUNNER_OUTPUT}" | grep -q 'ran 0 tests'; then
    printf 'FAIL: empty suite did not report the zero-test diagnosis\n'
    exit 1
fi
printf 'PASS: an empty suite still fails closed after widening\n'

printf 'All zero-test scope fallback checks passed.\n'
