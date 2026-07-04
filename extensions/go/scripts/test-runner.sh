#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHARED_LIB_DIR="${HOMEBOY_SHARED_LIB_DIR:-}"
if [ -z "$SHARED_LIB_DIR" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
    SHARED_LIB_DIR="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
fi
SHARED_LIB_DIR="${SHARED_LIB_DIR:-$(cd "${SCRIPT_DIR}/../../scripts/lib" && pwd)}"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/runner-harness.sh"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/test-failures-adapter.sh"
homeboy_runner_harness_init --sidecar-writer
homeboy_runner_harness_temp OUTPUT_FILE "homeboy-go-test.XXXXXX"

set +e
(cd "$PROJECT_PATH" && go test -json ./...) 2>&1 | tee "$OUTPUT_FILE"
TEST_EXIT=${PIPESTATUS[0]}
set -e

if homeboy_test_failures_enabled; then
    homeboy_runner_harness_temp TEST_FAILURES_TMP "homeboy-go-test-failures.XXXXXX"
    python3 - "$PROJECT_PATH" "$OUTPUT_FILE" "$TEST_FAILURES_TMP" <<'PY'
import hashlib
import json
import os
import sys

project, output_file, target = sys.argv[1:]
events = []
with open(output_file, encoding="utf-8") as handle:
    for raw in handle:
        try:
            events.append(json.loads(raw))
        except json.JSONDecodeError:
            continue

output_by_test = {}
failed_tests = []
failed_packages = set()
for event in events:
    package = event.get("Package", "")
    test = event.get("Test", "")
    action = event.get("Action", "")
    if event.get("Output") and test:
        output_by_test.setdefault((package, test), []).append(event["Output"].rstrip())
    if action == "fail" and test:
        failed_tests.append((package, test))
    elif action == "fail" and package:
        failed_packages.add(package)

failures = []
seen = set()
for package, test in failed_tests:
    if (package, test) in seen:
        continue
    seen.add((package, test))
    lines = output_by_test.get((package, test), [])
    message = next((line.strip() for line in reversed(lines) if line.strip()), f"{test} failed")
    test_id = f"{package}.{test}" if package else test
    identity = f"go:test:{test_id}:{message}"
    failures.append({
        "test_id": test_id,
        "suite": package or None,
        "file": None,
        "line": None,
        "message": message,
        "failure_type": "test_failure",
        "fingerprint": hashlib.sha256(identity.encode()).hexdigest(),
        "stdout_excerpt": "\n".join(lines)[-4000:] if lines else "",
        "stderr_excerpt": "",
    })

if not failures:
    for package in sorted(failed_packages):
        identity = f"go:package:{package}:failed"
        failures.append({
            "test_id": package,
            "suite": package,
            "file": None,
            "line": None,
            "message": f"go test package failed: {package}",
            "failure_type": "infrastructure",
            "fingerprint": hashlib.sha256(identity.encode()).hexdigest(),
            "stdout_excerpt": "",
            "stderr_excerpt": "",
        })

with open(target, "w", encoding="utf-8") as handle:
    json.dump(failures, handle, indent=2)
    handle.write("\n")
PY
    homeboy_test_failures_merge_file "$TEST_FAILURES_TMP"
fi

exit "$TEST_EXIT"
