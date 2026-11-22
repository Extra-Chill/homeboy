#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
# Standalone smoke runs do not have Homeboy's projected runtime helpers.
# Use the real settings accessor unless the caller supplied one, and export it
# for every test-runner subprocess below.
export HOMEBOY_RUNTIME_SETTINGS_HELPER="${HOMEBOY_RUNTIME_SETTINGS_HELPER:-$ROOT_DIR/../crates/homeboy-core/src/extension/runtime/settings.sh}"
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

[lib]
name = "renamed_lib"
path = "src/lib.rs"

[[test]]
name = "renamed_integration"
path = "tests/integration_scope.rs"
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
    assert_eq!(renamed_lib::value(), 1);
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
    HOMEBOY_TEST_RUNNER_ARGS=$'--test\nrenamed_integration' \
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

# Package-level candidates select every target and report the full denominator.
python3 - "$SCRIPT_DIR/test-shard-inventory.py" "$PROJECT_DIR" "$WORKDIR" <<'PY'
import importlib.util
import json
import sys
from pathlib import Path
sys.path.insert(0, str(Path(sys.argv[1]).parent))
spec = importlib.util.spec_from_file_location("inventory", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
current = module.inventory(sys.argv[2], "cargo", None)
package = current["tests"][0]["package"]
selection = Path(sys.argv[3]) / "package-selection.json"
selection.write_text(json.dumps({"schema": module.CHANGED_SELECTION_SCHEMA,
                                 "candidates": [{"package": package}]}))
resolved = module.resolve_changed_selection(selection, current, sys.argv[2])
expected = [test for test in current["tests"] if test["package"] == package]
assert {test["id"] for test in resolved["selected"]} == {test["id"] for test in expected}
assert resolved["selected_count"] == len(expected)
assert resolved["total_count"] == len(current["tests"])
assert len({test["target_kind"] for test in expected}) > 1
PY

cat > "$WORKDIR/changed-selection.json" <<'EOF'
{"schema":"homeboy/rust-changed-test-selection/v2","candidates":[{"package":"stale-producer-name","target_kind":"lib","target":"rust_changed_scope_smoke","module":"core::daemon::daemon_test","path":"src/core/daemon.rs"},{"package":"stale-producer-name","target_kind":"test","target":"integration_scope","module":null,"path":"tests/integration_scope.rs"}]}
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

SCOPED_INVENTORY="$WORKDIR/scoped-inventory.json"
HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_TEST_SCOPE_KIND='rust_changed_union' \
HOMEBOY_RUST_CHANGED_TEST_SELECTION_FILE="$WORKDIR/changed-selection.json" \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$SCOPED_INVENTORY" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
bash "$SCRIPT_DIR/test-runner.sh" >/dev/null
python3 - "$SCOPED_INVENTORY" "$WORKDIR/scoped-manifest.json" <<'PY'
import hashlib
import json
import sys

inventory = json.load(open(sys.argv[1]))
assert inventory["schema"] == "homeboy/test-inventory/v1", inventory
assert {test["name"] for test in inventory["tests"]} == {
    "core::daemon::daemon_test::inline_scope_runs",
    "integration_scope_runs",
}, inventory
fingerprint_record = {key: value for key, value in inventory.items() if key != "inventory_fingerprint"}
expected = hashlib.sha256(json.dumps(fingerprint_record, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
assert inventory["inventory_fingerprint"] == expected, inventory
selected_tests = inventory["tests"][:1]
shard_record = dict(fingerprint_record, tests=selected_tests)
shard_fingerprint = hashlib.sha256(json.dumps(shard_record, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
manifest = {key: inventory[key] for key in ("runner", "runner_fingerprint", "workspace_fingerprint")}
manifest["schema"] = "homeboy/test-shard-manifest/v1"
manifest["inventory_fingerprint"] = shard_fingerprint
manifest["tests"] = [test["id"] for test in selected_tests]
json.dump(manifest, open(sys.argv[2], "w"))
PY

# A manifest planned from a scoped inventory remains authoritative during
# replay even though replay enumerates the complete current inventory.
OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_RUST_TEST_RUNNER=nextest \
    HOMEBOY_TEST_SHARD_MANIFEST="$WORKDIR/scoped-manifest.json" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)
if [[ "$OUTPUT" != *"inline_scope_runs"* || "$OUTPUT" == *"integration_scope_runs"* || "$OUTPUT" == *"second_inline_scope_runs"* ]]; then
    printf 'Expected scoped inventory shard manifest to replay its exact subset. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

# Renames and deletions can leave a candidate absent from the current inventory.
# They must widen safely instead of returning a green zero-test result.
cat > "$WORKDIR/changed-selection.json" <<'EOF'
{"schema":"homeboy/rust-changed-test-selection/v2","candidates":[{"package":"rust-changed-scope-smoke","target_kind":"lib","target":"renamed_or_deleted","module":"core::deleted_test","path":"src/core/deleted_test.rs"}]}
EOF

FULL_FALLBACK_INVENTORY="$WORKDIR/full-fallback-inventory.json"
HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_TEST_SCOPE_KIND='rust_changed_union' \
HOMEBOY_RUST_CHANGED_TEST_SELECTION_FILE="$WORKDIR/changed-selection.json" \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$FULL_FALLBACK_INVENTORY" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
bash "$SCRIPT_DIR/test-runner.sh" >/dev/null
python3 - "$FULL_FALLBACK_INVENTORY" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
assert inventory["schema"] == "homeboy/test-inventory/v1", inventory
assert {test["name"] for test in inventory["tests"]} == {
    "core::daemon::daemon_test::inline_scope_runs",
    "core::service::service_test::second_inline_scope_runs",
    "integration_scope_runs",
}, inventory
PY

set +e
HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_TEST_SHARD_MANIFEST="$WORKDIR/scoped-manifest.json" \
HOMEBOY_RUST_CHANGED_TEST_SELECTION_FILE="$WORKDIR/changed-selection.json" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
bash "$SCRIPT_DIR/test-runner.sh" >"$WORKDIR/conflicting-selection.out" 2>&1
CONFLICTING_SELECTION_EXIT=$?
set -e
if [ "$CONFLICTING_SELECTION_EXIT" -eq 0 ] || ! grep -q 'are mutually exclusive' "$WORKDIR/conflicting-selection.out"; then
    printf 'Expected shard manifest and changed selection to fail as conflicting configuration\n' >&2
    exit 1
fi
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

# New producers pass only the additive selection-file variable. An older
# consumer ignores it and retains its normal full-suite behavior.
OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_TEST_SCOPE_KIND='rust_changed_union' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)
if [[ "$OUTPUT" != *"second_inline_scope_runs ... ok"* || "$OUTPUT" != *"integration_scope_runs ... ok"* ]]; then
    printf 'Expected missing selection file to preserve old-consumer full suite behavior. Output:\n%s\n' "$OUTPUT" >&2
    exit 1
fi

# Old producers do not set the additive selection-file variable. A new
# consumer must preserve the ordinary full-suite command rather than treating
# an absent selection as zero selected tests.
OUTPUT=$(
    HOMEBOY_EXTENSION_PATH="$(cd "$SCRIPT_DIR/.." && pwd)" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_TEST_SCOPE_KIND='workspace' \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$HELPER_DIR/resolve-context.sh" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$HELPER_DIR/runner-steps.sh" \
    bash "$SCRIPT_DIR/test-runner.sh"
)
if [[ "$OUTPUT" != *"second_inline_scope_runs ... ok"* || "$OUTPUT" != *"integration_scope_runs ... ok"* ]]; then
    printf 'Expected absent old-producer selection to preserve the full suite. Output:\n%s\n' "$OUTPUT" >&2
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
