#!/usr/bin/env bash
set -euo pipefail

PROJECT_PATH="${HOMEBOY_COMPONENT_PATH:-$(pwd)}"

write_lint_findings() {
    local gofmt_file="$1"
    local govet_file="$2"

    if [ -z "${HOMEBOY_LINT_FINDINGS_FILE:-}" ]; then
        return 0
    fi

    python3 - "$PROJECT_PATH" "$gofmt_file" "$govet_file" "$HOMEBOY_LINT_FINDINGS_FILE" <<'PY'
import hashlib
import json
import os
import re
import sys

project, gofmt_file, govet_file, target = sys.argv[1:]

def rel(path):
    if os.path.isabs(path):
        try:
            return os.path.relpath(path, project)
        except ValueError:
            return path
    return path

def excerpt(file, line):
    try:
        with open(os.path.join(project, file), encoding="utf-8") as handle:
            lines = handle.read().splitlines()
        if 1 <= line <= len(lines):
            return lines[line - 1][:240]
    except OSError:
        return None
    return None

findings = []
with open(gofmt_file, encoding="utf-8") as handle:
    for raw_file in handle.read().splitlines():
        file = rel(raw_file.strip())
        if not file:
            continue
        identity = f"go:gofmt:{file}:1"
        findings.append({
            "id": identity,
            "file": file,
            "line": 1,
            "column": 1,
            "severity": "warning",
            "source": "gofmt",
            "code": "formatting",
            "category": "format",
            "message": "File is not gofmt-formatted",
            "fixable": True,
            "fingerprint": hashlib.sha1(identity.encode()).hexdigest(),
            "excerpt": excerpt(file, 1),
        })

vet_pattern = re.compile(r"^(?P<file>[^:\n]+\.go):(?P<line>\d+):(?:(?P<column>\d+):)?\s*(?P<message>.+)$")
with open(govet_file, encoding="utf-8") as handle:
    for line in handle.read().splitlines():
        match = vet_pattern.match(line.strip())
        if not match:
            continue
        file = rel(match.group("file"))
        line_no = int(match.group("line"))
        column = int(match.group("column") or 1)
        message = match.group("message")
        identity = f"go:govet:{file}:{line_no}:{column}:{message}"
        findings.append({
            "id": identity,
            "file": file,
            "line": line_no,
            "column": column,
            "severity": "error",
            "source": "go vet",
            "code": "vet",
            "category": "correctness",
            "message": message,
            "fixable": False,
            "fingerprint": hashlib.sha1(identity.encode()).hexdigest(),
            "excerpt": excerpt(file, line_no),
        })

with open(target, "w", encoding="utf-8") as handle:
    json.dump(findings, handle, indent=2)
    handle.write("\n")
PY
}

GOFMT_FILE="$(mktemp)"
GOVET_FILE="$(mktemp)"
trap 'rm -f "$GOFMT_FILE" "$GOVET_FILE"' EXIT

find "$PROJECT_PATH" -name '*.go' -not -path '*/vendor/*' -not -path '*/.git/*' -print0 | while IFS= read -r -d '' file; do
    gofmt -l "$file"
done > "$GOFMT_FILE" || true

set +e
(cd "$PROJECT_PATH" && go vet ./...) 2>&1 | tee "$GOVET_FILE"
GOVET_EXIT=${PIPESTATUS[0]}
set -e

write_lint_findings "$GOFMT_FILE" "$GOVET_FILE"

if [ -s "$GOFMT_FILE" ]; then
    echo "Go files need formatting:"
    sed 's/^/  /' "$GOFMT_FILE"
    exit 1
fi

exit "$GOVET_EXIT"
