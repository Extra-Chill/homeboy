#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

PROJECT_DIR="$TMP_DIR/project"
mkdir -p "$PROJECT_DIR/src"

cat > "$PROJECT_DIR/Cargo.toml" <<'EOF'
[package]
name = "homeboy-release-prepare-fixture"
version = "0.1.0"
edition = "2021"
EOF

cat > "$PROJECT_DIR/src/lib.rs" <<'EOF'
pub fn answer() -> u8 {
    42
}
EOF

(cd "$PROJECT_DIR" && cargo generate-lockfile --quiet)
perl -0pi -e 's/version = "0\.1\.0"/version = "0.1.1"/' "$PROJECT_DIR/Cargo.toml"

(cd "$PROJECT_DIR" && bash "$ROOT_DIR/rust/scripts/prepare-release.sh" >/dev/null)

if ! grep -A2 'name = "homeboy-release-prepare-fixture"' "$PROJECT_DIR/Cargo.lock" | grep -q 'version = "0.1.1"'; then
  echo "Cargo.lock was not updated to the package release version" >&2
  exit 1
fi

echo "rust release prepare smoke passed"
