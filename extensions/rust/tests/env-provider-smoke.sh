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

env_for() {
    local project="$1"
    shift
    env \
        HOMEBOY_COMPONENT_ID=fixture \
        HOMEBOY_COMPONENT_PATH="$project" \
        HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
        XDG_DATA_HOME="$WORK_DIR/data" \
        "$@" \
        "$EXTENSION_DIR/scripts/env-provider.sh"
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
toolchain_env="$(env_for "$primary")"

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

cargo_home="$(printf '%s' "$toolchain_env" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("CARGO_HOME", ""))')"
rustup_home="$(printf '%s' "$toolchain_env" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("RUSTUP_HOME", ""))')"
if [ "$cargo_home" != "$HOME/.cargo" ] || [ "$rustup_home" != "$HOME/.rustup" ]; then
    printf 'Expected Rust toolchain homes from the runtime environment, got CARGO_HOME=%s RUSTUP_HOME=%s\n' "$cargo_home" "$rustup_home" >&2
    exit 1
fi

explicit_output="$(
    CARGO_TARGET_DIR="$explicit" \
    HOMEBOY_COMPONENT_ID=fixture \
    HOMEBOY_COMPONENT_PATH="$primary" \
    "$EXTENSION_DIR/scripts/env-provider.sh"
)"
explicit_target="$(printf '%s' "$explicit_output" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("CARGO_TARGET_DIR", ""))')"
explicit_homeboy_target="$(printf '%s' "$explicit_output" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("HOMEBOY_CARGO_TARGET_DIR", ""))')"
if [ "$explicit_target" != "$explicit" ] || [ "$explicit_homeboy_target" != "$explicit" ]; then
    printf 'Expected explicit CARGO_TARGET_DIR to remain the provider target, got %s\n' "$explicit_output" >&2
    exit 1
fi

accel_output="$(
    bin_dir="$WORK_DIR/bin"
    mkdir -p "$bin_dir"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$bin_dir/sccache"
    chmod +x "$bin_dir/sccache"
    PATH="$bin_dir:$PATH" \
        HOMEBOY_RUST_SCCACHE=1 \
        env_for "$primary"
)"

if [ "$(printf '%s' "$accel_output" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("RUSTC_WRAPPER", ""))')" != "sccache" ]; then
    printf 'Expected env provider to set RUSTC_WRAPPER=sccache, got %s\n' "$accel_output" >&2
    exit 1
fi

if [ "$(printf '%s' "$accel_output" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("HOMEBOY_RUST_SCCACHE_STATUS", ""))')" != "enabled" ]; then
    printf 'Expected sccache status enabled, got %s\n' "$accel_output" >&2
    exit 1
fi

printf 'Rust env provider smoke passed\n'
