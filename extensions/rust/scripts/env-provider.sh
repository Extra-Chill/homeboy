#!/usr/bin/env bash
set -euo pipefail

project_path="${HOMEBOY_COMPONENT_PATH:-${PWD}}"

if [ -n "${CARGO_TARGET_DIR:-}" ] || [ ! -f "${project_path}/Cargo.toml" ]; then
    printf '{}\n'
    exit 0
fi

normalize_identity() {
    local value="$1"
    value="${value%/}"
    value="${value%.git}"
    case "$value" in
        git@github.com:*) value="https://github.com/${value#git@github.com:}" ;;
        git@github.a8c.com:*) value="https://github.a8c.com/${value#git@github.a8c.com:}" ;;
    esac
    printf '%s' "$value" | tr '[:upper:]' '[:lower:]'
}

hash_identity() {
    if command -v shasum >/dev/null 2>&1; then
        printf '%s' "$1" | shasum -a 256 | awk '{print $1}'
    elif command -v sha256sum >/dev/null 2>&1; then
        printf '%s' "$1" | sha256sum | awk '{print $1}'
    else
        printf '%s' "$1" | openssl dgst -sha256 | awk '{print $NF}'
    fi
}

identity="component:${HOMEBOY_COMPONENT_ID:-component}"
if origin="$(git -C "$project_path" config --get remote.origin.url 2>/dev/null)" && [ -n "$origin" ]; then
    identity="$(normalize_identity "$origin")"
fi

label="$(printf '%s' "${HOMEBOY_COMPONENT_ID:-component}" | sed -E 's/[^A-Za-z0-9._-]+/_/g; s/^_+//; s/_+$//')"
if [ -z "$label" ]; then
    label="component"
fi

hash="$(hash_identity "$identity")"
data_dir="${HOMEBOY_DATA_DIR:-${XDG_DATA_HOME:-${HOME}/.local/share}/homeboy}"
target_dir="${data_dir}/cargo-targets/${label}-${hash:0:12}"

python3 - "$target_dir" <<'PY'
import json
import sys

target_dir = sys.argv[1]
print(json.dumps({
    "CARGO_TARGET_DIR": target_dir,
    "HOMEBOY_CARGO_TARGET_DIR": target_dir,
}))
PY
