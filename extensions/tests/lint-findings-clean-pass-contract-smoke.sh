#!/usr/bin/env bash
# A clean lint run still owes homeboy its declared lint.findings sidecar.
#
# Every extension that declares `"lint.findings": true` promises homeboy the
# sidecar on *all* exit paths. Runners that only wrote it when a tool reported
# something left a clean pass with no evidence at all, and homeboy rejects
# missing evidence as an `internal.io_error` harness failure rather than reading
# it as "nothing to report" — so a green lint became a red gate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT}/.." && pwd)/homeboy}"
CORE_RUNTIME_DIR="${HOMEBOY_CORE_DIR}/crates/homeboy-core/src/extension/runtime"
SIDECAR_WRITER_HELPER="${HOMEBOY_RUNTIME_SIDECAR_WRITER:-${CORE_RUNTIME_DIR}/sidecar-writer.sh}"
RUNNER_PRELUDE_HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${CORE_RUNTIME_DIR}/runner-prelude.sh}"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${CORE_RUNTIME_DIR}/runner-steps.sh}"
COMMAND_CAPTURE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-${CORE_RUNTIME_DIR}/command-capture.sh}"
LINT_FINDINGS_ADAPTER="${ROOT}/scripts/lib/lint-findings-adapter.sh"

# The static contract below needs nothing but this repo, so it runs everywhere.
# The behavioural checks drive real runners and need homeboy's runtime helpers,
# which are only present when a core checkout is available.
RUNTIME_HELPERS_AVAILABLE=1
for helper in "$SIDECAR_WRITER_HELPER" "$RUNNER_PRELUDE_HELPER" "$RUNNER_STEPS_HELPER" "$COMMAND_CAPTURE_HELPER"; do
    [ -f "$helper" ] || RUNTIME_HELPERS_AVAILABLE=0
done

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

# ── Contract: every declaring extension seeds the sidecar ──
#
# Asserted statically so a new lint runner cannot quietly reintroduce the gap in
# a language this smoke has no toolchain for.
python3 - "$ROOT" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
lint_runners = {
    "rust": "rust/scripts/lint-runner.sh",
    "go": "go/scripts/lint-runner.sh",
    "swift": "swift/scripts/lint-runner.sh",
    "nodejs": "nodejs/scripts/lint/lint-runner.sh",
    "wordpress": "wordpress/scripts/lint/lint-runner.sh",
}
# nodejs seeds the sidecar inline on each of its clean exit paths instead of
# through the shared initializer; either discharges the contract.
seeders = ("homeboy_lint_findings_init", "homeboy_lint_findings_write_empty")

declared = []
for manifest in sorted(root.glob("*/*.json")):
    if manifest.stem != manifest.parent.name:
        continue
    try:
        data = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        continue
    if not isinstance(data, dict):
        continue
    if data.get("structured_sidecars", {}).get("lint.findings") is True:
        declared.append(manifest.parent.name)

assert declared, "no extension declares lint.findings; contract test is inert"

failures = []
for extension in declared:
    relative = lint_runners.get(extension)
    if relative is None:
        failures.append(f"{extension}: declares lint.findings but has no known lint runner mapping")
        continue
    runner = root / relative
    if not runner.is_file():
        failures.append(f"{extension}: missing lint runner {relative}")
        continue
    body = runner.read_text(encoding="utf-8")
    if not any(seeder in body for seeder in seeders):
        failures.append(
            f"{extension}: {relative} declares lint.findings but never seeds it; "
            "a clean pass would leave no evidence"
        )

if failures:
    print("\n".join(failures), file=sys.stderr)
    sys.exit(1)

print(f"ok lint.findings seeding contract ({', '.join(declared)})")
PY

if [ "$RUNTIME_HELPERS_AVAILABLE" -ne 1 ]; then
    echo "skip behavioural checks: homeboy runtime helpers unavailable (set HOMEBOY_CORE_DIR)"
    echo "lint-findings-clean-pass-contract-smoke: PASS"
    exit 0
fi

# ── Unit: the shared initializer seeds without ever clobbering ──
(
    # shellcheck source=/dev/null
    source "$SIDECAR_WRITER_HELPER"
    # shellcheck source=/dev/null
    source "$LINT_FINDINGS_ADAPTER"

    HOMEBOY_LINT_FINDINGS_FILE="$TMP_DIR/seeded.json"
    export HOMEBOY_LINT_FINDINGS_FILE
    homeboy_lint_findings_init
    [ "$(tr -d '[:space:]' < "$TMP_DIR/seeded.json")" = "[]" ]

    # Idempotent, and a populated sidecar survives a later init.
    printf '[{"tool":"phpcs","message":"kept"}]' > "$TMP_DIR/populated.json"
    HOMEBOY_LINT_FINDINGS_FILE="$TMP_DIR/populated.json"
    homeboy_lint_findings_init
    homeboy_lint_findings_init
    grep -q '"kept"' "$TMP_DIR/populated.json"

    # A seeded sidecar still accepts the merges runners perform on findings.
    HOMEBOY_LINT_FINDINGS_FILE="$TMP_DIR/merged.json"
    homeboy_lint_findings_init
    printf '[{"tool":"clippy","message":"finding"}]' > "$TMP_DIR/incoming.json"
    homeboy_lint_findings_merge_file "$TMP_DIR/incoming.json"
    python3 -c "
import json, sys
data = json.load(open(sys.argv[1]))
assert data == [{'tool': 'clippy', 'message': 'finding'}], data
" "$TMP_DIR/merged.json"

    # No sidecar requested stays a no-op rather than writing to an empty path.
    unset HOMEBOY_LINT_FINDINGS_FILE
    homeboy_lint_findings_init
)
echo "ok lint findings initializer"

# ── End to end: a clean Rust lint leaves an empty findings sidecar ──
#
# A stub cargo reports success for every step, which is the exact shape that
# used to produce zero evidence.
RUST_PROJECT="$TMP_DIR/rust-project"
RUST_BIN="$TMP_DIR/rust-bin"
mkdir -p "$RUST_PROJECT/src" "$RUST_BIN"
cat > "$RUST_PROJECT/Cargo.toml" <<'EOF'
[package]
name = "clean"
version = "0.1.0"
edition = "2021"
EOF
cat > "$RUST_PROJECT/src/lib.rs" <<'EOF'
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
EOF
cat > "$RUST_BIN/cargo" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$RUST_BIN/cargo"

RUST_LINT_FINDINGS="$TMP_DIR/rust-clean-lint-findings.json"
set +e
PATH="$RUST_BIN:$PATH" \
HOMEBOY_EXTENSION_PATH="$ROOT/rust" \
HOMEBOY_COMPONENT_PATH="$RUST_PROJECT" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="$RUNNER_STEPS_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_LINT_FINDINGS_FILE="$RUST_LINT_FINDINGS" \
bash "$ROOT/rust/scripts/lint-runner.sh" >/dev/null 2>&1
RUST_LINT_EXIT=$?
set -e

if [ "$RUST_LINT_EXIT" -ne 0 ]; then
    echo "Expected a clean Rust lint to exit 0, got ${RUST_LINT_EXIT}" >&2
    exit 1
fi
if [ ! -f "$RUST_LINT_FINDINGS" ]; then
    echo "Clean Rust lint produced no lint.findings sidecar: $RUST_LINT_FINDINGS" >&2
    exit 1
fi
python3 -c "
import json, sys
data = json.load(open(sys.argv[1]))
assert data == [], data
" "$RUST_LINT_FINDINGS"
echo "ok clean rust lint emits empty findings sidecar"

echo "lint-findings-clean-pass-contract-smoke: PASS"
