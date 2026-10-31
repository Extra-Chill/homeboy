#!/usr/bin/env bash
#
# Smoke test: the ESLint runner hands its JSON report to node on stdin, never
# as an argv string. Linux caps a single argument at 128 KiB (MAX_ARG_STRLEN),
# so a large report ("node: Argument list too long", exit 126) used to abort
# the runner and surface as a lint infrastructure failure (exit 2) on any
# component with enough JS findings.

set -euo pipefail

RUNNER="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/scripts/lint/eslint-runner.sh"

if grep -nE "'[[:space:]]+\"\\\$json_output\"" "$RUNNER"; then
    echo "FAIL: eslint-runner.sh passes \$json_output to node as an argument" >&2
    exit 1
fi
if grep -n 'JSON.parse(process.argv' "$RUNNER"; then
    echo "FAIL: eslint-runner.sh parses the ESLint report from argv" >&2
    exit 1
fi

if command -v node >/dev/null 2>&1; then
    # A >128 KiB report must round-trip through the stdin parse used by the runner.
    big=$(node -e 'const m=[];for(let i=0;i<4000;i++)m.push({ruleId:"no-unused-vars",message:"x".repeat(40)});process.stdout.write(JSON.stringify([{filePath:"/a.js",messages:m,errorCount:4000,warningCount:0}]))')
    count=$(node -e 'const d=JSON.parse(require("fs").readFileSync(0,"utf8"));process.stdout.write(String(d[0].messages.length))' <<<"$big")
    [ "$count" = "4000" ] || { echo "FAIL: stdin parse returned '$count'" >&2; exit 1; }
fi

echo "eslint-runner JSON stdin smoke passed"
