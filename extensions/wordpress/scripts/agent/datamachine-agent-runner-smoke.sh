#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"

node "$EXTENSION_PATH/tests/datamachine-agent-task-runner-smoke.js"

echo "✓ Data Machine agent runner smoke test PASSED via WP Codebox"
