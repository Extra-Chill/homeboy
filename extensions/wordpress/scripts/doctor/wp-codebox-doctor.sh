#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: wp-codebox-doctor.sh [doctor|cleanup] [wp-codebox doctor options]

Thin Homeboy wrapper for upstream WP Codebox health commands.

Commands:
  doctor   Report WP Codebox runner health. Default.
  cleanup  Run doctor checks and remove safe stale/corrupt runtime state.

Common options passed through to WP Codebox:
  --fix                      Allow mutating cleanup when command is doctor.
  --stale-after-seconds N    Age threshold for stale recipe-run processes.
  --archive-root DIR         Additional archive/cache root to scan. Repeatable.
  --json                     Emit WP Codebox JSON health output.

Environment:
  HOMEBOY_WP_CODEBOX_BIN    Specific wp-codebox binary.
  HOMEBOY_SETTINGS_JSON     May provide wp_codebox_bin.
USAGE
}

settings_wp_codebox_bin() {
    [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] || return 0
    [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ] || return 0

    if command -v jq >/dev/null 2>&1; then
        printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true
        return 0
    fi

    if command -v node >/dev/null 2>&1; then
        node -e '
const input = process.env.HOMEBOY_SETTINGS_JSON || "{}";
try {
  const value = JSON.parse(input).wp_codebox_bin;
  if (typeof value === "string") process.stdout.write(value);
} catch {}
' 2>/dev/null || true
    fi
}

resolve_wp_codebox_bin() {
    local bin="${HOMEBOY_WP_CODEBOX_BIN:-}"
    if [ -z "$bin" ]; then
        bin="$(settings_wp_codebox_bin)"
    fi
    bin="${bin:-wp-codebox}"

    if [[ "$bin" = */* ]]; then
        printf '%s\n' "$bin"
        return 0
    fi

    command -v "$bin" 2>/dev/null || printf '%s\n' "$bin"
}

MODE="doctor"
if [ "$#" -gt 0 ]; then
    case "$1" in
        doctor|cleanup)
            MODE="$1"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
    esac
fi

WP_CODEBOX_BIN="$(resolve_wp_codebox_bin)"
exec "$WP_CODEBOX_BIN" "$MODE" "$@"
