#!/usr/bin/env bash
# WP Codebox-owned test-result parser adapters.

homeboy_parse_wp_codebox_test_results() {
    local output_file="$1"

    if [ -z "${output_file:-}" ] || [ ! -f "$output_file" ]; then
        return 1
    fi

    if ! type homeboy_write_test_results >/dev/null 2>&1; then
        return 1
    fi

    local parsed
    parsed=$(python3 - "$output_file" <<'PY'
import json
import sys

output_file = sys.argv[1]

try:
    with open(output_file, encoding="utf-8") as handle:
        text = handle.read()
except OSError:
    sys.exit(1)

if '"schema"' not in text or '"wp-codebox/test-results/v1"' not in text:
    sys.exit(1)

try:
    data = json.loads(text)
except json.JSONDecodeError:
    sys.exit(1)

if not isinstance(data, dict) or data.get("schema") != "wp-codebox/test-results/v1":
    sys.exit(1)

summary = data.get("summary") if isinstance(data.get("summary"), dict) else {}
total = int(summary.get("total") or 0)
passed = int(summary.get("passed") or 0)
failed = int(summary.get("failed") or 0)
skipped = int(summary.get("skipped") or 0)
unknown = int(summary.get("unknown") or 0)

if total == 0 and isinstance(data.get("suites"), list):
    for suite in data["suites"]:
        if not isinstance(suite, dict):
            continue
        total += int(suite.get("tests") or suite.get("total") or 0)
        passed += int(suite.get("passed") or 0)
        failed += int(suite.get("failed") or 0)
        skipped += int(suite.get("skipped") or 0)
        unknown += int(suite.get("unknown") or 0)

partial = "wp-codebox-unknown" if data.get("status") == "unknown" or unknown > 0 else ""
print("\t".join(str(value) for value in (total, passed, failed, skipped, partial)))
PY
    ) || return 1

    [ -n "$parsed" ] || return 1

    local total passed failed skipped partial
    IFS=$'\t' read -r total passed failed skipped partial <<EOF
$parsed
EOF

    homeboy_write_test_results "${total:-0}" "${passed:-0}" "${failed:-0}" "${skipped:-0}" "${partial:-}"
}
