#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FINGERPRINT_SCRIPT="$SCRIPT_DIR/scripts/fingerprint.sh"

python3 - "$FINGERPRINT_SCRIPT" <<'PY'
import json
import subprocess
import sys

fingerprint_script = sys.argv[1]


def fingerprint(content):
    payload = json.dumps({"file_path": "inc/Cli/Bootstrap.php", "content": content})
    result = subprocess.run(
        [fingerprint_script],
        input=payload,
        text=True,
        capture_output=True,
        check=True,
    )
    return json.loads(result.stdout)


registered = fingerprint(
    r'''<?php
namespace DataMachine\Cli;

WP_CLI::add_command( 'datamachine email', Commands\EmailCommand::class );
'''
)

assert registered["runtime_dispatched_types"] == ["Commands\\EmailCommand"], registered
assert "datamachine email" in registered["registrations"], registered

docblock_only = fingerprint(
    r'''<?php
namespace DataMachine\Cli\Commands;

class EmailCommand {
    /**
     * Test the IMAP connection.
     *
     * @subcommand test-connection
     */
    public function test_connection( array $args, array $assoc_args ): void {}
}
'''
)

assert docblock_only["runtime_dispatched_types"] == [], docblock_only

print("wordpress wp-cli runtime dispatch fingerprint smoke passed")
PY
