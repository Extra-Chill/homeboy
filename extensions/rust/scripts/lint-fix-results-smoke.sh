#!/usr/bin/env bash
# Smoke-test Rust lint fix-results sidecar emission without requiring cargo fixes.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

PROJECT_DIR="${TMP_DIR}/project"
FAKE_BIN="${TMP_DIR}/bin"
RESULTS_FILE="${TMP_DIR}/fix-results.json"
mkdir -p "${PROJECT_DIR}/src" "${FAKE_BIN}"

cat > "${PROJECT_DIR}/Cargo.toml" <<'TOML'
[package]
name = "homeboy-rust-fix-results-smoke"
version = "0.1.0"
edition = "2021"
TOML

cat > "${PROJECT_DIR}/src/lib.rs" <<'RS'
pub fn value() -> &'static str { "FMT_BEFORE CLIPPY_BEFORE FIX_BEFORE" }
RS

git -C "${PROJECT_DIR}" init >/dev/null
git -C "${PROJECT_DIR}" add Cargo.toml src/lib.rs
git -C "${PROJECT_DIR}" \
    -c user.name="Homeboy Smoke" \
    -c user.email="homeboy-smoke@example.com" \
    commit -m "fixture" >/dev/null

cat > "${FAKE_BIN}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

cmd="${1:-}"
case "$cmd" in
    fmt)
        perl -0pi -e 's/FMT_BEFORE/FMT_AFTER/g' "${HOMEBOY_COMPONENT_PATH}/src/lib.rs"
        ;;
    clippy)
        perl -0pi -e 's/CLIPPY_BEFORE/CLIPPY_AFTER/g' "${HOMEBOY_COMPONENT_PATH}/src/lib.rs"
        ;;
    fix)
        perl -0pi -e 's/FIX_BEFORE/FIX_AFTER/g' "${HOMEBOY_COMPONENT_PATH}/src/lib.rs"
        ;;
    *)
        echo "unexpected cargo command: $*" >&2
        exit 2
        ;;
esac
SH
chmod +x "${FAKE_BIN}/cargo"

PATH="${FAKE_BIN}:${PATH}" \
HOMEBOY_COMPONENT_PATH="${PROJECT_DIR}" \
HOMEBOY_EXTENSION_PATH="${ROOT_DIR}/rust" \
HOMEBOY_RUNTIME_RUNNER_STEPS="${ROOT_DIR}/wordpress/scripts/lib/runner-steps.sh" \
HOMEBOY_FIX_ONLY=1 \
HOMEBOY_FIX_RESULTS_FILE="${RESULTS_FILE}" \
    bash "${SCRIPT_DIR}/lint-runner.sh" >/dev/null

python3 - "${RESULTS_FILE}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)

expected = [
    {"file": "src/lib.rs", "rule": "rustfmt", "action": "format"},
    {"file": "src/lib.rs", "rule": "clippy", "action": "rewrite"},
    {"file": "src/lib.rs", "rule": "cargo_fix", "action": "rewrite"},
]

if data != expected:
    raise SystemExit(f"unexpected fix results: {data!r} != {expected!r}")
PY

echo "Rust lint fix-results smoke passed"
