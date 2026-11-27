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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../lib/wp-codebox-paths.sh"

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

WP_CODEBOX_BIN="$(homeboy_wp_codebox_resolve_bin "${HOMEBOY_SETTINGS_JSON:-}")"
homeboy_wp_codebox_set_command "$WP_CODEBOX_BIN"
homeboy_wp_codebox_preflight_command
# Keep upstream JSON on stdout intact while surfacing extension/cache drift.
PINNED_REF="$(jq -r '.wp_codebox.ref // empty' "${SCRIPT_DIR}/../../wordpress.json")"
if [[ "${PINNED_REF}" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    OBSERVED_VERSION="$("${HOMEBOY_WP_CODEBOX_COMMAND[@]}" --version 2>/dev/null || true)"
    OBSERVED_VERSION="$(printf '%s' "${OBSERVED_VERSION}" | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?' | head -n 1 || true)"
    if [ "${OBSERVED_VERSION}" != "${PINNED_REF#v}" ]; then
        echo "WP Codebox pin/cache mismatch: expected ${PINNED_REF}, observed ${OBSERVED_VERSION:-unavailable}. Run homeboy extension setup wordpress." >&2
    fi
fi
exec "${HOMEBOY_WP_CODEBOX_COMMAND[@]}" "$MODE" "$@"
