#!/usr/bin/env bash
set -euo pipefail

# Member-crate tests must actually execute on a Cargo workspace component.
#
# At a hybrid root (a Cargo.toml that is both [package] and [workspace]),
# `cargo test` without `--workspace` runs ONLY the root package's targets and
# silently skips every member crate. Member crates are still compiled as test
# targets, so nothing looks broken: the binaries are built and never run.
#
# `--workspace` used to be added only for `workspace`/`full` scope kinds, so
# every other kind (`args`, `rust_filter`, `rust_integration`) inherited the
# root-package-only default. On the homeboy repository that hid ten genuinely
# failing member-crate tests from CI indefinitely (#10477).
#
# Guards: member tests run for each non-full scope kind, an explicitly narrow
# scope stays narrow, and the resolved selection is always reported.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORK_DIR="$(mktemp -d -t homeboy-rust-ws-scope.XXXXXX)"
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

# A hybrid root: the root Cargo.toml is both a [package] and a [workspace], with
# one member crate whose test is uniquely named so we can prove it executed.
make_workspace() {
    local dir="$1"
    mkdir -p "${dir}/src" "${dir}/crates/member/src"

    cat > "${dir}/Cargo.toml" <<'TOML'
[package]
name = "hybrid-root"
version = "0.1.0"
edition = "2021"

[workspace]
members = ["crates/member"]

[dependencies]
member = { path = "crates/member" }
TOML

    cat > "${dir}/src/lib.rs" <<'RS'
pub fn root_value() -> i32 { 1 }

#[cfg(test)]
mod tests {
    #[test]
    fn root_package_test_runs() {
        assert_eq!(super::root_value(), 1);
    }
}
RS

    cat > "${dir}/crates/member/Cargo.toml" <<'TOML'
[package]
name = "member"
version = "0.1.0"
edition = "2021"
TOML

    cat > "${dir}/crates/member/src/lib.rs" <<'RS'
pub fn member_value() -> i32 { 2 }

#[cfg(test)]
mod tests {
    #[test]
    fn member_crate_test_runs() {
        assert_eq!(super::member_value(), 2);
    }
}
RS
}

run_runner() {
    local project="$1" scope_kind="$2" runner_args="$3"
    set +e
    HOMEBOY_EXTENSION_PATH="${EXTENSION_DIR}" \
        HOMEBOY_COMPONENT_PATH="${project}" \
        HOMEBOY_SKIP_LINT=1 \
        HOMEBOY_TEST_SCOPE_KIND="${scope_kind}" \
        HOMEBOY_TEST_RUNNER_ARGS="${runner_args}" \
        HOMEBOY_RUNTIME_RUNNER_PRELUDE="${WORK_DIR}/runner-prelude.sh" \
        HOMEBOY_RUNTIME_COMMAND_CAPTURE="${WORK_DIR}/command-capture.sh" \
        bash "${EXTENSION_DIR}/scripts/test-runner.sh" >"${WORK_DIR}/out.log" 2>&1
    RUNNER_EXIT=$?
    set -e
    RUNNER_OUTPUT="$(cat "${WORK_DIR}/out.log")"
}

make_workspace "${WORK_DIR}/ws"

# 1-3. Every non-full scope kind must still reach the member crate.
for kind in args rust_filter rust_integration; do
    run_runner "${WORK_DIR}/ws" "${kind}" ''
    if [ "${RUNNER_EXIT}" -ne 0 ]; then
        printf 'FAIL: scope kind %s exited %s\n' "${kind}" "${RUNNER_EXIT}"
        printf '%s\n' "${RUNNER_OUTPUT}"
        exit 1
    fi
    if ! printf '%s' "${RUNNER_OUTPUT}" | grep -q 'member_crate_test_runs ... ok'; then
        printf 'FAIL: scope kind %s did not execute the member crate test\n' "${kind}"
        printf '%s\n' "${RUNNER_OUTPUT}"
        exit 1
    fi
    if ! printf '%s' "${RUNNER_OUTPUT}" | grep -q 'root_package_test_runs ... ok'; then
        printf 'FAIL: scope kind %s stopped running the root package test\n' "${kind}"
        exit 1
    fi
    printf 'PASS: scope kind %s executes member crate and root package tests\n' "${kind}"
done

# 4. An explicitly narrow scope must stay narrow: `-p` selects packages itself,
#    so `--workspace` must not be forced on top of it.
run_runner "${WORK_DIR}/ws" 'rust_filter' "$(printf -- '-p\nmember\n--lib')"
if [ "${RUNNER_EXIT}" -ne 0 ]; then
    printf 'FAIL: package-scoped run exited %s\n' "${RUNNER_EXIT}"
    printf '%s\n' "${RUNNER_OUTPUT}"
    exit 1
fi
if printf '%s' "${RUNNER_OUTPUT}" | grep -q 'root_package_test_runs'; then
    printf 'FAIL: -p member must not widen to the root package\n'
    printf '%s\n' "${RUNNER_OUTPUT}"
    exit 1
fi
if ! printf '%s' "${RUNNER_OUTPUT}" | grep -q 'member_crate_test_runs ... ok'; then
    printf 'FAIL: -p member did not run the member crate test\n'
    exit 1
fi
printf 'PASS: an explicitly package-scoped run stays narrow\n'

# 5. The resolved selection must always be auditable from the log.
run_runner "${WORK_DIR}/ws" 'args' ''
for needle in 'Rust test scope kind:' 'Rust test package selection:' 'Rust test invocation: cargo test'; do
    if ! printf '%s' "${RUNNER_OUTPUT}" | grep -q "${needle}"; then
        printf 'FAIL: runner did not report %s\n' "${needle}"
        printf '%s\n' "${RUNNER_OUTPUT}"
        exit 1
    fi
done
if ! printf '%s' "${RUNNER_OUTPUT}" | grep -q 'Rust test invocation: .*--workspace'; then
    printf 'FAIL: reported invocation did not include --workspace\n'
    exit 1
fi
printf 'PASS: resolved scope kind, package selection, and cargo invocation are reported\n'

printf '\nAll workspace member test scope checks passed.\n'
