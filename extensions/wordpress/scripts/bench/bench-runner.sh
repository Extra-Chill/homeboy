#!/usr/bin/env bash
set -euo pipefail

# Bench runner entrypoint for WordPress Homeboy extension.
#
# Bench workloads run through WP Codebox so WordPress runtime behavior uses the
# same sandbox/artifact contract as tests and agent CI.

if ((BASH_VERSINFO[0] < 4)); then
    echo "============================================" >&2
    echo "ERROR: bash 4.0+ required (found ${BASH_VERSION})" >&2
    echo "============================================" >&2
    case "$(uname -s)" in
        Darwin)
            echo "macOS ships bash 3.2. Fix: brew install bash" >&2
            echo "Then restart your terminal (Homebrew bash takes priority on PATH)." >&2
            ;;
        *)
            echo "Update bash via your package manager." >&2
            ;;
    esac
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "${SCRIPT_DIR}/bench-runner-wp-codebox.sh" "$@"
