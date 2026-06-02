#!/usr/bin/env bash
set -euo pipefail

# Bench runner entrypoint for WordPress Homeboy extension.
#
# Bench workloads run through WP Codebox so WordPress runtime behavior uses the
# same sandbox/artifact contract as tests and agent CI.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASH_PREFLIGHT_HELPER="${HOMEBOY_RUNTIME_BASH_PREFLIGHT:?Homeboy core must provide HOMEBOY_RUNTIME_BASH_PREFLIGHT}"
# shellcheck source=/dev/null
source "$BASH_PREFLIGHT_HELPER"
homeboy_require_bash_version 4

exec bash "${SCRIPT_DIR}/bench-runner-wp-codebox.sh" "$@"
