#!/usr/bin/env bash

# Core-materialized normalized LINT-FINDING record emitter for extension runners.
#
# Every per-language lint runner (rust/go/swift/nodejs/wordpress) hand-builds the
# same normalized finding record: a stable identity, a sha1 fingerprint of that
# identity, and a 240-character excerpt of the offending source line. This shim
# owns that record shape in one place so the per-language runners can route
# through it and delete their copies with byte-identical output.
#
# Usage:
#   source "${HOMEBOY_RUNTIME_EMIT_LINT_FINDING:-/path/to/fallback}"
#   homeboy_emit_lint_finding \
#       --root "$PROJECT_PATH" \
#       --id "rust:clippy:src/lib.rs:10:5:unused:unused variable" \
#       --file src/lib.rs --line 10 --column 5 \
#       --severity warning --source clippy --code unused \
#       --category correctness --message "unused variable" --fixable false
#
# Prints one normalized LINT-FINDING JSON object (compact, single line) on stdout
# with this exact key order — the shape per-language runners append/merge into the
# lint findings sidecar:
#   {"id":..,"file":..,"line":..,"column":..,"severity":..,"source":..,
#    "code":..,"category":..,"message":..,"fixable":<bool>,
#    "fingerprint":<sha1(id)>,"excerpt":<line text truncated to 240 chars|null>}
#
# Arguments:
#   --root      project root used to resolve --file when reading the excerpt
#   --id        stable identity string (REQUIRED); the fingerprint is sha1(id)
#   --file      finding file path, relative to --root (used for the excerpt)
#   --line      1-based line number (default 0; excerpt is null when out of range)
#   --column    1-based column number (default 1)
#   --severity  severity label (default "warning")
#   --source    tool/source label (e.g. clippy, gofmt, eslint)
#   --code      rule/code identifier
#   --category  normalized category (e.g. format, correctness, style)
#   --message   human-readable finding message
#   --fixable   "true"/"false" (default "false")
#
# The fingerprint is sha1 of --id and the excerpt is the first 240 characters of
# the source line, matching the per-language builders byte-for-byte.

homeboy_emit_lint_finding_python() {
    if command -v python3 >/dev/null 2>&1; then
        command -v python3
        return 0
    fi
    if command -v python >/dev/null 2>&1; then
        command -v python
        return 0
    fi
    echo "[emit-lint-finding] python3 or python is required" >&2
    return 1
}

homeboy_emit_lint_finding() {
    local lf_root="" lf_id="" lf_file="" lf_line="0" lf_column="1" \
        lf_severity="warning" lf_source="" lf_code="" lf_category="" \
        lf_message="" lf_fixable="false"

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --root) lf_root="${2:-}"; shift 2 ;;
            --id) lf_id="${2:-}"; shift 2 ;;
            --file) lf_file="${2:-}"; shift 2 ;;
            --line) lf_line="${2:-0}"; shift 2 ;;
            --column) lf_column="${2:-1}"; shift 2 ;;
            --severity) lf_severity="${2:-}"; shift 2 ;;
            --source) lf_source="${2:-}"; shift 2 ;;
            --code) lf_code="${2:-}"; shift 2 ;;
            --category) lf_category="${2:-}"; shift 2 ;;
            --message) lf_message="${2:-}"; shift 2 ;;
            --fixable) lf_fixable="${2:-false}"; shift 2 ;;
            *)
                echo "[emit-lint-finding] unknown argument: $1" >&2
                return 2
                ;;
        esac
    done

    if [ -z "$lf_id" ]; then
        echo "[emit-lint-finding] --id is required" >&2
        return 2
    fi

    local python_bin
    python_bin="$(homeboy_emit_lint_finding_python)" || return 1

    HOMEBOY_LF_ROOT="$lf_root" \
    HOMEBOY_LF_ID="$lf_id" \
    HOMEBOY_LF_FILE="$lf_file" \
    HOMEBOY_LF_LINE="$lf_line" \
    HOMEBOY_LF_COLUMN="$lf_column" \
    HOMEBOY_LF_SEVERITY="$lf_severity" \
    HOMEBOY_LF_SOURCE="$lf_source" \
    HOMEBOY_LF_CODE="$lf_code" \
    HOMEBOY_LF_CATEGORY="$lf_category" \
    HOMEBOY_LF_MESSAGE="$lf_message" \
    HOMEBOY_LF_FIXABLE="$lf_fixable" \
    "$python_bin" <<'PYEOF'
import hashlib
import json
import os
import sys


def env_int(name, default=0):
    raw = os.environ.get(name, "")
    try:
        return int(raw)
    except (TypeError, ValueError):
        return default


root = os.environ.get("HOMEBOY_LF_ROOT", "")
identity = os.environ.get("HOMEBOY_LF_ID", "")
file = os.environ.get("HOMEBOY_LF_FILE", "")
line = env_int("HOMEBOY_LF_LINE", 0)
column = env_int("HOMEBOY_LF_COLUMN", 1)
fixable = os.environ.get("HOMEBOY_LF_FIXABLE", "false").strip().lower() in (
    "1",
    "true",
    "yes",
)


def excerpt(root, file, line):
    if not file or line <= 0:
        return None
    try:
        with open(os.path.join(root, file), encoding="utf-8") as handle:
            lines = handle.read().splitlines()
    except OSError:
        return None
    if 1 <= line <= len(lines):
        return lines[line - 1][:240]
    return None


record = {
    "id": identity,
    "file": file,
    "line": line,
    "column": column,
    "severity": os.environ.get("HOMEBOY_LF_SEVERITY", ""),
    "source": os.environ.get("HOMEBOY_LF_SOURCE", ""),
    "code": os.environ.get("HOMEBOY_LF_CODE", ""),
    "category": os.environ.get("HOMEBOY_LF_CATEGORY", ""),
    "message": os.environ.get("HOMEBOY_LF_MESSAGE", ""),
    "fixable": fixable,
    "fingerprint": hashlib.sha1(identity.encode("utf-8")).hexdigest(),
    "excerpt": excerpt(root, file, line),
}

json.dump(record, sys.stdout, separators=(",", ":"), ensure_ascii=False)
sys.stdout.write("\n")
PYEOF
}
