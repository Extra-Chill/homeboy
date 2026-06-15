#!/usr/bin/env bash

# Rust-owned toolchain acceleration knobs. Homeboy core only asks the extension
# for environment metadata; all Rust/Cargo/sccache/linker choices live here.

homeboy_rust_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|on|ON|auto|AUTO) return 0 ;;
        *) return 1 ;;
    esac
}

homeboy_rust_setting() {
    local setting_name="${1:?setting name is required}"
    local expression="${2:-.$setting_name}"
    local default_value="${3:-}"

    if [ -z "${HOMEBOY_SETTINGS_JSON:-}" ] || ! command -v jq >/dev/null 2>&1; then
        printf '%s' "$default_value"
        return 0
    fi

    local value
    value=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r "$expression" 2>/dev/null || true)
    case "$value" in
        ""|null) printf '%s' "$default_value" ;;
        *) printf '%s' "$value" ;;
    esac
}

homeboy_rust_sccache_request() {
    if [ -n "${HOMEBOY_RUST_SCCACHE:-}" ]; then
        printf '%s' "$HOMEBOY_RUST_SCCACHE"
        return 0
    fi

    homeboy_rust_setting rust_sccache '.rust_sccache // .rust.sccache // false' 'false'
}

homeboy_rust_linker_request() {
    if [ -n "${HOMEBOY_RUST_LINKER:-}" ]; then
        printf '%s' "$HOMEBOY_RUST_LINKER"
        return 0
    fi

    homeboy_rust_setting rust_linker '.rust_linker // .rust.linker // empty' ''
}

homeboy_rust_linker_supported() {
    local linker="$1"
    case "$linker" in
        mold)
            command -v mold >/dev/null 2>&1
            ;;
        lld)
            command -v ld.lld >/dev/null 2>&1 || command -v lld >/dev/null 2>&1
            ;;
        *)
            return 1
            ;;
    esac
}

homeboy_rust_linker_flag() {
    case "$1" in
        mold) printf '%s' '-C link-arg=-fuse-ld=mold' ;;
        lld) printf '%s' '-C link-arg=-fuse-ld=lld' ;;
    esac
}

homeboy_rust_toolchain_env_json() {
    local sccache_request sccache_status sccache_note
    local linker_request linker_status linker_note linker_flag rustflags
    local rustc_wrapper="${RUSTC_WRAPPER:-}"

    sccache_request="$(homeboy_rust_sccache_request)"
    sccache_status="off"
    sccache_note=""
    if homeboy_rust_truthy "$sccache_request"; then
        if [ -n "$rustc_wrapper" ]; then
            sccache_status="external"
            sccache_note="RUSTC_WRAPPER was already set; the Rust extension did not override it."
        elif command -v sccache >/dev/null 2>&1; then
            rustc_wrapper="sccache"
            sccache_status="enabled"
        else
            sccache_status="unavailable"
            sccache_note="sccache was requested but is not on PATH."
        fi
    fi

    linker_request="$(homeboy_rust_linker_request)"
    linker_status="off"
    linker_note=""
    rustflags="${RUSTFLAGS:-}"
    case "$linker_request" in
        ""|off|OFF|none|NONE|false|FALSE|0)
            ;;
        mold|lld)
            if homeboy_rust_linker_supported "$linker_request"; then
                linker_flag="$(homeboy_rust_linker_flag "$linker_request")"
                case " $rustflags " in
                    *" $linker_flag "*) ;;
                    *) rustflags="${rustflags:+$rustflags }$linker_flag" ;;
                esac
                linker_status="$linker_request"
            else
                linker_status="unavailable"
                linker_note="${linker_request} linker was requested but no matching linker binary was found on PATH."
            fi
            ;;
        *)
            linker_status="unsupported"
            linker_note="Unsupported HOMEBOY_RUST_LINKER value: ${linker_request}."
            ;;
    esac

    python3 - "$rustc_wrapper" "$sccache_status" "$sccache_note" "$linker_status" "$linker_note" "$rustflags" <<'PY'
import json
import sys

rustc_wrapper, sccache_status, sccache_note, linker_status, linker_note, rustflags = sys.argv[1:]
env = {
    "HOMEBOY_RUST_SCCACHE_STATUS": sccache_status,
    "HOMEBOY_RUST_LINKER_STATUS": linker_status,
}
if rustc_wrapper:
    env["RUSTC_WRAPPER"] = rustc_wrapper
if rustflags:
    env["RUSTFLAGS"] = rustflags
if sccache_note:
    env["HOMEBOY_RUST_SCCACHE_NOTE"] = sccache_note
if linker_note:
    env["HOMEBOY_RUST_LINKER_NOTE"] = linker_note
print(json.dumps(env, sort_keys=True))
PY
}

homeboy_rust_toolchain_metadata_json() {
    python3 - <<'PY'
import json
import os

metadata = {
    "target_dir": os.environ.get("CARGO_TARGET_DIR") or os.environ.get("HOMEBOY_CARGO_TARGET_DIR") or "",
    "sccache_status": os.environ.get("HOMEBOY_RUST_SCCACHE_STATUS", "external" if os.environ.get("RUSTC_WRAPPER") else "off"),
    "linker_status": os.environ.get("HOMEBOY_RUST_LINKER_STATUS", "external" if "-fuse-ld=" in os.environ.get("RUSTFLAGS", "") else "off"),
}
if os.environ.get("RUSTC_WRAPPER"):
    metadata["rustc_wrapper"] = os.environ["RUSTC_WRAPPER"]
if os.environ.get("RUSTFLAGS"):
    metadata["rustflags"] = os.environ["RUSTFLAGS"]
if os.environ.get("HOMEBOY_RUST_SCCACHE_NOTE"):
    metadata["sccache_note"] = os.environ["HOMEBOY_RUST_SCCACHE_NOTE"]
if os.environ.get("HOMEBOY_RUST_LINKER_NOTE"):
    metadata["linker_note"] = os.environ["HOMEBOY_RUST_LINKER_NOTE"]
print(json.dumps(metadata, sort_keys=True))
PY
}
