#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORK_DIR="$(mktemp -d -t homeboy-rust-env-provider.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT

make_project() {
    local dir="$1"
    mkdir -p "$dir/src"
    cat > "$dir/Cargo.toml" <<'TOML'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
TOML
    printf 'fn main() {}\n' > "$dir/src/main.rs"
}

target_dir_for() {
    local project="$1"
    HOMEBOY_COMPONENT_ID=fixture \
        HOMEBOY_COMPONENT_PATH="$project" \
        HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
        XDG_DATA_HOME="$WORK_DIR/data" \
        "$EXTENSION_DIR/scripts/env-provider.sh" \
        | python3 -c 'import json,sys; print(json.load(sys.stdin).get("CARGO_TARGET_DIR", ""))'
}

primary="$WORK_DIR/primary"
worktree="$WORK_DIR/worktree"
explicit="$WORK_DIR/explicit-target"
make_project "$primary"
cp -R "$primary" "$worktree"
git -C "$primary" init -q
git -C "$primary" remote add origin https://github.com/Extra-Chill/homeboy.git
git -C "$worktree" init -q
git -C "$worktree" remote add origin git@github.com:Extra-Chill/homeboy.git

primary_target="$(target_dir_for "$primary")"
worktree_target="$(target_dir_for "$worktree")"

if [ -z "$primary_target" ] || [ "$primary_target" != "$worktree_target" ]; then
    printf 'Expected matching shared target dirs, got primary=%s worktree=%s\n' "$primary_target" "$worktree_target" >&2
    exit 1
fi

case "$primary_target" in
    "$WORK_DIR/data/homeboy/cargo-targets/fixture-"*) ;;
    *)
        printf 'Expected target under Homeboy data dir, got %s\n' "$primary_target" >&2
        exit 1
        ;;
esac

explicit_output="$(
    CARGO_TARGET_DIR="$explicit" \
    HOMEBOY_COMPONENT_ID=fixture \
    HOMEBOY_COMPONENT_PATH="$primary" \
    "$EXTENSION_DIR/scripts/env-provider.sh"
)"
if [ "$explicit_output" != '{}' ]; then
    printf 'Expected explicit CARGO_TARGET_DIR to suppress provider output, got %s\n' "$explicit_output" >&2
    exit 1
fi

printf 'Rust env provider smoke passed\n'
