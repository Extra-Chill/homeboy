#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-artifacts.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

ARTIFACT_DIR="$TMP_DIR/artifact"
RESULTS_FILE="$TMP_DIR/test-results-sidecar.json"
FAILURES_FILE="$TMP_DIR/test-failures-sidecar.json"
WRITE_RESULTS_HELPER="$TMP_DIR/write-test-results.sh"
mkdir -p "$ARTIFACT_DIR/files" "$ARTIFACT_DIR/logs"

cat > "$ARTIFACT_DIR/files/test-results.json" <<'JSON'
{
  "schema": "wp-codebox/test-results/v1",
  "status": "failed",
  "summary": { "total": 3, "passed": 1, "failed": 1, "skipped": 1, "unknown": 0 },
  "suites": [
    {
      "name": "phpunit",
      "status": "failed",
      "tests": 3,
      "passed": 1,
      "failed": 1,
      "skipped": 1,
      "unknown": 0,
      "testCases": [
        {
          "name": "DataMachine\\Tests\\Unit\\Abilities\\FileAbilitiesTest::test_list_files_rejects_both_scopes",
          "status": "failed",
          "message": "Failed asserting that 'Flow step 8-6 not found' contains \"Cannot provide both\".",
          "file": "/tmp/plugin/inc/Abilities/FileAbilities.php",
          "line": 94,
          "testFile": "/tmp/plugin/tests/Unit/Abilities/FileAbilitiesTest.php"
        }
      ]
    }
  ],
  "rawLogReferences": [
    { "path": "commands.jsonl", "kind": "commands-jsonl" },
    { "path": "logs/commands.log", "kind": "commands-log" },
    { "path": "logs/runtime.log", "kind": "runtime-log" }
  ]
}
JSON

cat > "$ARTIFACT_DIR/commands.jsonl" <<'JSONL'
{"command":"phpunit","exitCode":1,"summary":"Tests: 3, Assertions: 4, Failures: 1, Skipped: 1."}
JSONL

cat > "$ARTIFACT_DIR/logs/commands.log" <<'LOG'
phpunit --configuration phpunit.xml.dist
Tests: 3, Assertions: 4, Failures: 1, Skipped: 1.
LOG

cat > "$ARTIFACT_DIR/logs/runtime.log" <<'LOG'
WP Codebox runtime completed with failed test status.
LOG

cat > "$WRITE_RESULTS_HELPER" <<'SH'
function homeboy_write_test_results {
    local total="$1"
    local passed="$2"
    local failed="$3"
    local skipped="$4"
    local partial="${5:-}"

    php -r '
        $payload = array(
            "total" => (int) $argv[2],
            "passed" => (int) $argv[3],
            "failed" => (int) $argv[4],
            "skipped" => (int) $argv[5],
        );
        if ($argv[6] !== "") {
            $payload["partial"] = $argv[6];
        }
        file_put_contents($argv[1], json_encode($payload, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . PHP_EOL);
    ' "$HOMEBOY_TEST_RESULTS_FILE" "$total" "$passed" "$failed" "$skipped" "$partial"
}
SH

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$RESULTS_FILE" \
    bash "$SCRIPT_DIR/parse-test-results.sh" "$ARTIFACT_DIR"

HOMEBOY_TEST_FAILURES_FILE="$FAILURES_FILE" \
    bash "$SCRIPT_DIR/parse-test-failures.sh" "$ARTIFACT_DIR" "/tmp/plugin"

python3 - "$RESULTS_FILE" "$FAILURES_FILE" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    results = json.load(handle)

assert results == {"total": 3, "passed": 1, "failed": 1, "skipped": 1}, results

with open(sys.argv[2], encoding="utf-8") as handle:
    failures_payload = json.load(handle)

assert failures_payload["total"] == 3, failures_payload
assert failures_payload["passed"] == 1, failures_payload
failures = failures_payload["failures"]
assert len(failures) == 1, failures
failure = failures[0]

expected = {
    "test_id": "DataMachine\\Tests\\Unit\\Abilities\\FileAbilitiesTest::test_list_files_rejects_both_scopes",
    "suite": "phpunit",
    "file": "inc/Abilities/FileAbilities.php",
    "line": 94,
    "message": "Failed asserting that 'Flow step 8-6 not found' contains \"Cannot provide both\".",
    "failure_type": "AssertionFailedError",
    "test_file": "tests/Unit/Abilities/FileAbilitiesTest.php",
}

for key, value in expected.items():
    assert failure.get(key) == value, f"{key}: {failure.get(key)!r} != {value!r}"

assert failure["test_name"] == failure["test_id"]
assert failure["error_type"] == failure["failure_type"]
assert failure["source_file"] == failure["file"]
assert failure["source_line"] == failure["line"]
assert len(failure.get("fingerprint", "")) == 64
assert "commands.log" in failure.get("stdout_excerpt", "")
assert "runtime completed" in failure.get("stdout_excerpt", "")
assert failure.get("stderr_excerpt") == ""

print("wordpress parse wp-codebox artifacts smoke passed")
PY
