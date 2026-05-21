#!/usr/bin/env bash
set -euo pipefail

PROJECT_PATH="${HOMEBOY_COMPONENT_PATH:-$(pwd)}"
OUTPUT_FILE="$(mktemp)"
trap 'rm -f "$OUTPUT_FILE"' EXIT

set +e
(cd "$PROJECT_PATH" && go test -json ./...) 2>&1 | tee "$OUTPUT_FILE"
TEST_EXIT=${PIPESTATUS[0]}
set -e

if [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ]; then
    python3 - "$PROJECT_PATH" "$OUTPUT_FILE" "$HOMEBOY_TEST_FAILURES_FILE" <<'PY'
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
fi

exit "$TEST_EXIT"
