#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner.sh"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PLUGIN_PATH="${TMPDIR}/component"
FAKE_WP_CODEBOX="${TMPDIR}/wp-codebox.js"
mkdir -p "${PLUGIN_PATH}/tests"

printf '<?php
class BootstrapFailureTest extends WP_UnitTestCase {}
' > "${PLUGIN_PATH}/tests/BootstrapFailureTest.php"

cat > "$FAKE_WP_CODEBOX" <<'NODE'
#!/usr/bin/env node
process.stdout.write(JSON.stringify({
  success: true,
  executions: [
    {
      stdout: 'PHPUnit 9.6.34 by Sebastian Bergmann and contributors.\n\nError in bootstrap script: Error:\nInterface "AgentsAPI\\Core\\Database\\Chat\\WP_Agent_Conversation_Store" not found\n#0 /wordpress/wp-content/plugins/data-machine/data-machine.php(440)\n',
      stderr: '',
    },
  ],
}) + '\n')
NODE
chmod +x "$FAKE_WP_CODEBOX"

set +e
output=$(HOMEBOY_WP_CODEBOX_BIN="$FAKE_WP_CODEBOX" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="example" \
    bash "$RUNNER" 2>&1)
status=$?
set -e

if [ "$status" -eq 0 ]; then
    echo "Expected WP Codebox runner to fail on PHPUnit bootstrap error" >&2
    echo "$output" >&2
    exit 1
fi

if [[ "$output" != *"PHPUNIT BOOTSTRAP FAILURE"* ]]; then
    echo "Expected PHPUnit bootstrap failure diagnostics" >&2
    echo "$output" >&2
    exit 1
fi

if [[ "$output" != *"Error in bootstrap script:"* ]]; then
    echo "Expected original PHPUnit bootstrap error in output" >&2
    echo "$output" >&2
    exit 1
fi

echo "WP Codebox bootstrap failure smoke passed"
