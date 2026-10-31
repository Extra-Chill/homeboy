#!/usr/bin/env bash
set -euo pipefail

# Bench runner entrypoint for WordPress Homeboy extension.
#
# Bench workloads run through a runtime backend. WP Codebox is the default
# backend and preserves the existing sandbox/artifact contract.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASH_PREFLIGHT_HELPER="${HOMEBOY_RUNTIME_BASH_PREFLIGHT:?Homeboy core must provide HOMEBOY_RUNTIME_BASH_PREFLIGHT}"
WP_CODEBOX_RUNNER="${HOMEBOY_RUNTIME_BENCH_RUNNER_WP_CODEBOX:-${SCRIPT_DIR}/bench-runner-wp-codebox.sh}"
# shellcheck source=/dev/null
source "$BASH_PREFLIGHT_HELPER"
homeboy_require_bash_version 4

BENCH_RUNTIME_BACKEND="${HOMEBOY_WORDPRESS_BENCH_RUNTIME_BACKEND:-wp-codebox}"

case "$BENCH_RUNTIME_BACKEND" in
    wp-codebox)
        exec bash "$WP_CODEBOX_RUNNER" "$@"
        ;;
    *)
        echo "ERROR: unsupported WordPress bench runtime backend: ${BENCH_RUNTIME_BACKEND}" >&2
        echo "Supported WordPress bench runtime backends: wp-codebox" >&2
        exit 2
        ;;
esac
