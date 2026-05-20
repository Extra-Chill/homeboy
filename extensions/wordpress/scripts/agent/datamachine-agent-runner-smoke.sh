#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

bash "$SCRIPT_DIR/datamachine-agent-wp-codebox-runner-smoke.sh"

echo "✓ Data Machine agent runner smoke test PASSED via WP Codebox"
