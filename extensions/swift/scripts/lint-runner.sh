#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context

echo "Running Swift lint for: $(basename "$COMPONENT_PATH")"

write_swiftlint_findings() {
    local input_file="$1"

    if [ -z "${HOMEBOY_LINT_FINDINGS_FILE:-}" ] || [ ! -f "$input_file" ]; then
        return 0
    fi

    python3 - "$COMPONENT_PATH" "$input_file" "$HOMEBOY_LINT_FINDINGS_FILE" <<'PY'
import hashlib
import json
import os
import sys

component, input_file, target = sys.argv[1:]
try:
    with open(input_file, encoding="utf-8") as handle:
        diagnostics = json.load(handle)
except (OSError, json.JSONDecodeError):
    diagnostics = []

def rel(path):
    if os.path.isabs(path):
        try:
            return os.path.relpath(path, component)
        except ValueError:
            return path
    return path

def excerpt(file, line):
    try:
        with open(os.path.join(component, file), encoding="utf-8") as handle:
            lines = handle.read().splitlines()
        if 1 <= line <= len(lines):
            return lines[line - 1][:240]
    except OSError:
        return None
    return None

findings = []
for diagnostic in diagnostics if isinstance(diagnostics, list) else []:
    file = rel(str(diagnostic.get("file", "")))
    line = int(diagnostic.get("line") or 1)
    column = int(diagnostic.get("character") or 1)
    rule = str(diagnostic.get("rule_id") or "swiftlint")
    message = str(diagnostic.get("reason") or diagnostic.get("message") or "SwiftLint finding")
    severity = str(diagnostic.get("severity") or "warning")
    identity = f"swift:swiftlint:{file}:{line}:{column}:{rule}:{message}"
    findings.append({
        "id": identity,
        "file": file,
        "line": line,
        "column": column,
        "severity": "error" if severity == "error" else "warning",
        "source": "swiftlint",
        "code": rule,
        "category": "style",
        "message": message,
        "fixable": bool(diagnostic.get("correction")),
        "fingerprint": hashlib.sha1(identity.encode()).hexdigest(),
        "excerpt": excerpt(file, line),
    })

with open(target, "w", encoding="utf-8") as handle:
    json.dump(findings, handle, indent=2)
    handle.write("\n")
PY
}

if command -v swiftlint >/dev/null 2>&1; then
    SWIFTLINT_JSON="$(mktemp)"
    trap 'rm -f "$SWIFTLINT_JSON"' EXIT
    set +e
    swiftlint lint --path "$COMPONENT_PATH" --reporter json > "$SWIFTLINT_JSON"
    SWIFTLINT_EXIT=$?
    set -e
    write_swiftlint_findings "$SWIFTLINT_JSON"
    cat "$SWIFTLINT_JSON"
    exit "$SWIFTLINT_EXIT"
elif command -v swiftformat >/dev/null 2>&1; then
    swiftformat "$COMPONENT_PATH" --lint
else
    echo "Swift lint skipped: install SwiftLint or SwiftFormat to enable Swift linting."
fi
