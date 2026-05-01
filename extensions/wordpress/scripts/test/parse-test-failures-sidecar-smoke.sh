#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-sidecar.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

PHPUNIT_OUTPUT="$TMP_DIR/phpunit-output.txt"
FAILURES_FILE="$TMP_DIR/failures.json"

cat > "$PHPUNIT_OUTPUT" <<'EOF'
There was 1 failure:

1) DataMachine\Tests\Unit\Abilities\FileAbilitiesTest::test_list_files_rejects_both_scopes
Failed asserting that 'Flow step 8-6 not found' contains "Cannot provide both".

/tmp/plugin/inc/Abilities/FileAbilities.php:94
/tmp/plugin/tests/Unit/Abilities/FileAbilitiesTest.php:31

FAILURES!
Tests: 4, Assertions: 9, Failures: 1.
EOF

HOMEBOY_TEST_FAILURES_FILE="$FAILURES_FILE" \
    bash "$SCRIPT_DIR/parse-test-failures.sh" "$PHPUNIT_OUTPUT" "/tmp/plugin"

python3 - "$FAILURES_FILE" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)

assert payload["total"] == 4, payload
assert payload["passed"] == 3, payload
failures = payload["failures"]
assert len(failures) == 1, failures
failure = failures[0]

expected = {
    "test_id": "DataMachine\\Tests\\Unit\\Abilities\\FileAbilitiesTest::test_list_files_rejects_both_scopes",
    "suite": "phpunit",
    "file": "inc/Abilities/FileAbilities.php",
    "line": 94,
    "message": "Failed asserting that 'Flow step 8-6 not found' contains \"Cannot provide both\".",
    "failure_type": "AssertionFailedError",
}

for key, value in expected.items():
    assert failure.get(key) == value, f"{key}: {failure.get(key)!r} != {value!r}"

assert failure["test_name"] == failure["test_id"]
assert failure["error_type"] == failure["failure_type"]
assert failure["source_file"] == failure["file"]
assert failure["source_line"] == failure["line"]
assert len(failure.get("fingerprint", "")) == 64
assert "FileAbilitiesTest::test_list_files" in failure.get("stdout_excerpt", "")
assert failure.get("stderr_excerpt") == ""

print("wordpress parse-test-failures sidecar smoke passed")
PY
