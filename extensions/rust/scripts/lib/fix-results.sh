#!/usr/bin/env bash

# Shared fix-result capture helpers for extension runners.
#
# Extensions still decide which fixers run. This helper only owns the common
# mutation capture contract: snapshot tracked files before a fixer, compare
# after it runs, and append normalized FixApplied records to Homeboy's fix
# results sidecar.

HOMEBOY_FIX_RESULTS_JSON="${HOMEBOY_FIX_RESULTS_JSON:-[]}"

homeboy_fix_results_hash_file() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
        return 0
    fi
    sha256sum "$1" | cut -d ' ' -f 1
}

homeboy_fix_results_capture() {
    local target_file="$1"
    local root="${2:-${PROJECT_PATH:-${HOMEBOY_COMPONENT_PATH:-.}}}"
    shift 2 || true
    local pathspecs=("$@")

    if [ ${#pathspecs[@]} -eq 0 ]; then
        pathspecs=(.)
    fi

    if ! git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        : > "$target_file"
        return 0
    fi

    git -C "$root" ls-files -- "${pathspecs[@]}" | while IFS= read -r file; do
        [ -n "$file" ] || continue
        if [ -f "${root}/${file}" ]; then
            printf '%s\t%s\n' "$file" "$(homeboy_fix_results_hash_file "${root}/${file}")"
        fi
    done | sort > "$target_file"
}

homeboy_fix_results_append_changed() {
    local rule="$1"
    local action="$2"
    local before_file="$3"
    local confidence="${4:-}"
    local root="${5:-${PROJECT_PATH:-${HOMEBOY_COMPONENT_PATH:-.}}}"
    shift 5 || true
    local pathspecs=("$@")
    local after_file
    after_file="$(mktemp)"

    homeboy_fix_results_capture "$after_file" "$root" "${pathspecs[@]}"

    local changed_files
    changed_files="$(awk -F '\t' '
        NR == FNR { before[$1] = $2; next }
        !($1 in before) || before[$1] != $2 { print $1 }
    ' "$before_file" "$after_file")"
    rm -f "$after_file"

    if [ -z "$changed_files" ]; then
        return 0
    fi

    HOMEBOY_FIX_RESULTS_JSON="$(CHANGED_FILES="$changed_files" python3 - "$HOMEBOY_FIX_RESULTS_JSON" "$rule" "$action" "$confidence" <<'PY' 2>/dev/null || printf '%s' "$HOMEBOY_FIX_RESULTS_JSON"
import json
import os
import sys

results = json.loads(sys.argv[1])
rule = sys.argv[2]
action = sys.argv[3]
confidence = sys.argv[4]
for file in os.environ.get("CHANGED_FILES", "").splitlines():
    if not file:
        continue
    result = {"file": file, "rule": rule, "action": action}
    if confidence:
        result["confidence"] = confidence
    results.append(result)
print(json.dumps(results, separators=(",", ":")))
PY
)"
}

homeboy_fix_results_write() {
    if [ -z "${HOMEBOY_FIX_RESULTS_FILE:-}" ]; then
        return 0
    fi

    if ! type homeboy_write_fix_results >/dev/null 2>&1; then
        echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write fix results" >&2
        return 1
    fi

    HOMEBOY_FIX_RESULTS_JSON="$HOMEBOY_FIX_RESULTS_JSON" python3 - <<'PY' | while IFS= read -r result; do
import json
import os

for item in json.loads(os.environ.get("HOMEBOY_FIX_RESULTS_JSON", "[]")):
    print(json.dumps(item, separators=(",", ":")))
PY
        homeboy_append_fix_result "$result"
    done
}
