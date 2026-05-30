#!/usr/bin/env bash

# Shared JSON sidecar writer for extension runner scripts.
#
# The helper owns the common "sidecar is a JSON array" file mechanics used by
# lint findings, test failures, fix results, and annotation files. Callers still build the
# domain-specific JSON object; this helper validates it, writes atomically, and
# keeps empty/missing target env vars as no-ops for legacy compatibility.
#
# Usage:
#   source "${HOMEBOY_RUNTIME_SIDECAR_WRITER}"
#   homeboy_sidecar_emit lint.finding '{"message":"..."}'
#   homeboy_sidecar_write test.failures "$failure_json"
#   homeboy_sidecar_merge annotation.phpcs "$tmp_annotations"

homeboy_sidecar_python() {
    if command -v python3 >/dev/null 2>&1; then
        command -v python3
        return 0
    fi
    if command -v python >/dev/null 2>&1; then
        command -v python
        return 0
    fi
    echo "[sidecar-writer] python3 or python is required" >&2
    return 1
}

homeboy_sidecar_write_json_array() {
    local target="${1:-}"
    shift || true

    if [ -z "$target" ]; then
        return 0
    fi

    local python_bin
    python_bin="$(homeboy_sidecar_python)" || return 1

    "$python_bin" - "$target" "$@" <<'PYEOF'
import json
import os
import sys
import tempfile

target = sys.argv[1]
items = [json.loads(raw) for raw in sys.argv[2:]]
directory = os.path.dirname(target) or "."
os.makedirs(directory, exist_ok=True)
fd, tmp = tempfile.mkstemp(prefix=".homeboy-sidecar-", suffix=".json", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(items, handle, separators=(",", ":"), ensure_ascii=False)
        handle.write("\n")
    os.replace(tmp, target)
except Exception:
    try:
        os.unlink(tmp)
    except OSError:
        pass
    raise
PYEOF
}

homeboy_sidecar_append_json() {
    local target="${1:-}"
    local item="${2:-}"

    if [ -z "$target" ]; then
        return 0
    fi

    local python_bin
    python_bin="$(homeboy_sidecar_python)" || return 1

    "$python_bin" - "$target" "$item" <<'PYEOF'
import json
import os
import sys
import tempfile

target = sys.argv[1]
item = json.loads(sys.argv[2])
items = []
if os.path.exists(target) and os.path.getsize(target) > 0:
    with open(target, "r", encoding="utf-8") as handle:
        loaded = json.load(handle)
    if isinstance(loaded, list):
        items = loaded
    else:
        raise ValueError(f"sidecar must contain a JSON array: {target}")
items.append(item)
directory = os.path.dirname(target) or "."
os.makedirs(directory, exist_ok=True)
fd, tmp = tempfile.mkstemp(prefix=".homeboy-sidecar-", suffix=".json", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(items, handle, separators=(",", ":"), ensure_ascii=False)
        handle.write("\n")
    os.replace(tmp, target)
except Exception:
    try:
        os.unlink(tmp)
    except OSError:
        pass
    raise
PYEOF
}

homeboy_sidecar_merge_json_array() {
    local target="${1:-}"
    local source="${2:-}"

    if [ -z "$target" ] || [ -z "$source" ] || [ ! -s "$source" ]; then
        return 0
    fi

    local python_bin
    python_bin="$(homeboy_sidecar_python)" || return 1

    "$python_bin" - "$target" "$source" <<'PYEOF'
import json
import os
import sys
import tempfile

target = sys.argv[1]
source = sys.argv[2]
items = []
if os.path.exists(target) and os.path.getsize(target) > 0:
    with open(target, "r", encoding="utf-8") as handle:
        loaded = json.load(handle)
    if isinstance(loaded, list):
        items = loaded
    else:
        raise ValueError(f"sidecar must contain a JSON array: {target}")
with open(source, "r", encoding="utf-8") as handle:
    incoming = json.load(handle)
if not isinstance(incoming, list):
    raise ValueError(f"source sidecar must contain a JSON array: {source}")
items.extend(incoming)
directory = os.path.dirname(target) or "."
os.makedirs(directory, exist_ok=True)
fd, tmp = tempfile.mkstemp(prefix=".homeboy-sidecar-", suffix=".json", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(items, handle, separators=(",", ":"), ensure_ascii=False)
        handle.write("\n")
    os.replace(tmp, target)
except Exception:
    try:
        os.unlink(tmp)
    except OSError:
        pass
    raise
PYEOF
}

homeboy_sidecar_target_for_type() {
    local type="${1:-}"

    case "$type" in
        lint.finding|lint.findings)
            printf '%s\n' "${HOMEBOY_LINT_FINDINGS_FILE:-}"
            ;;
        test.failure|test.failures)
            printf '%s\n' "${HOMEBOY_TEST_FAILURES_FILE:-}"
            ;;
        fix.result|fix.results)
            printf '%s\n' "${HOMEBOY_FIX_RESULTS_FILE:-}"
            ;;
        annotation.*|annotations.*)
            local name="${type#annotation.}"
            name="${name#annotations.}"
            if [ -z "${HOMEBOY_ANNOTATIONS_DIR:-}" ] || [ -z "$name" ]; then
                printf '\n'
                return 0
            fi
            name="$(printf '%s' "$name" | tr -c 'A-Za-z0-9_.-' '-')"
            printf '%s/%s.json\n' "${HOMEBOY_ANNOTATIONS_DIR%/}" "$name"
            ;;
        *)
            echo "[sidecar-writer] unknown sidecar type: $type" >&2
            return 1
            ;;
    esac
}

homeboy_sidecar_write() {
    local type="${1:-}"
    shift || true

    local target
    target="$(homeboy_sidecar_target_for_type "$type")" || return 1
    homeboy_sidecar_write_json_array "$target" "$@"
}

homeboy_sidecar_emit() {
    local type="${1:-}"
    local item="${2:-}"

    local target
    target="$(homeboy_sidecar_target_for_type "$type")" || return 1
    homeboy_sidecar_append_json "$target" "$item"
}

homeboy_sidecar_merge() {
    local type="${1:-}"
    local source="${2:-}"

    local target
    target="$(homeboy_sidecar_target_for_type "$type")" || return 1
    homeboy_sidecar_merge_json_array "$target" "$source"
}

homeboy_annotation_file() {
    local source="${1:-}"
    homeboy_sidecar_target_for_type "annotation.$source"
}

homeboy_write_lint_findings() {
    homeboy_sidecar_write lint.findings "$@"
}

homeboy_append_lint_finding() {
    homeboy_sidecar_emit lint.finding "$1"
}

homeboy_merge_lint_findings() {
    homeboy_sidecar_merge lint.findings "$1"
}

homeboy_write_test_failures() {
    homeboy_sidecar_write test.failures "$@"
}

homeboy_append_test_failure() {
    homeboy_sidecar_emit test.failure "$1"
}

homeboy_merge_test_failures() {
    homeboy_sidecar_merge test.failures "$1"
}

homeboy_write_fix_results() {
    homeboy_sidecar_write fix.results "$@"
}

homeboy_append_fix_result() {
    homeboy_sidecar_emit fix.result "$1"
}

homeboy_merge_fix_results() {
    homeboy_sidecar_merge fix.results "$1"
}

homeboy_write_annotations() {
    local source="${1:-}"
    shift || true

    homeboy_sidecar_write "annotation.$source" "$@"
}

homeboy_append_annotation() {
    local source="${1:-}"
    local item="${2:-}"

    homeboy_sidecar_emit "annotation.$source" "$item"
}

homeboy_merge_annotations() {
    local source="${1:-}"
    local file="${2:-}"

    homeboy_sidecar_merge "annotation.$source" "$file"
}
