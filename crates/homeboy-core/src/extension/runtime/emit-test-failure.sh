#!/usr/bin/env bash

# Core-materialized normalized TEST-FAILURE record emitter for extension runners.
#
# Every per-language test runner (rust/go/swift/nodejs) and the WordPress
# parse-test-failures.php hand-build the same normalized failure record: a stable
# identity, a sha256 fingerprint of that identity, a bounded output excerpt, and
# an "infrastructure" fallback failure_type when no individual failures could be
# parsed. This shim owns that record shape in one place so the per-language
# runners can route through it and delete their copies with byte-identical output.
#
# Usage:
#   source "${HOMEBOY_RUNTIME_EMIT_TEST_FAILURE:-/path/to/fallback}"
#   homeboy_emit_test_failure \
#       --test-id "suite::case" \
#       --suite phpunit --file src/Foo.php --line 42 \
#       --failure-type AssertionError \
#       --message "Failed asserting that false is true" \
#       --identity "rust:test:suite::case" \
#       --stdout-excerpt "$(homeboy_test_failure_bound_excerpt "$RAW_OUTPUT")"
#
# Prints one normalized TEST-FAILURE JSON object (compact, single line) on stdout
# with this exact key order — the shape per-language runners append/merge into the
# test failures sidecar:
#   {"test_id":..,"suite":..|null,"file":..|null,"line":..|null,"message":..,
#    "failure_type":..,"fingerprint":<sha256(identity)>,
#    "stdout_excerpt":..,"stderr_excerpt":..}
#
# Arguments:
#   --test-id       fully-qualified test identifier (REQUIRED)
#   --suite         suite/framework label (empty -> null)
#   --file          normalized failure file path (empty -> null)
#   --line          1-based line number (omitted/empty -> null, "0" -> 0)
#   --message       human-readable failure message
#   --failure-type  failure classification (default "test_failure"); pass
#                   "infrastructure" for the infra-fallback record emitted when no
#                   individual failures could be parsed
#   --identity      identity string the fingerprint is sha256'd from. When omitted
#                   the canonical identity is derived as the NUL-joined tuple of
#                   test_id, file, line, failure_type, and the first message line
#                   (matching the WordPress make_failure_fingerprint contract).
#   --stdout-excerpt  bounded stdout excerpt (used verbatim; default "")
#   --stderr-excerpt  bounded stderr excerpt (used verbatim; default "")
#
# Bounded excerpts: callers may pre-bound their captured output, or pipe it
# through homeboy_test_failure_bound_excerpt, which reproduces the canonical
# WordPress make_output_excerpt bound (first 40 lines, trimmed, capped at 4000
# characters with a trailing ellipsis).

homeboy_emit_test_failure_python() {
    if command -v python3 >/dev/null 2>&1; then
        command -v python3
        return 0
    fi
    if command -v python >/dev/null 2>&1; then
        command -v python
        return 0
    fi
    echo "[emit-test-failure] python3 or python is required" >&2
    return 1
}

# Bound a raw output blob to the canonical sidecar excerpt shape: keep the first
# 40 lines, trim surrounding whitespace, and cap the result at 4000 characters
# (replacing the tail with "..."). Mirrors WordPress make_output_excerpt.
homeboy_test_failure_bound_excerpt() {
    local raw="${1:-}"

    local python_bin
    python_bin="$(homeboy_emit_test_failure_python)" || return 1

    HOMEBOY_TF_RAW="$raw" "$python_bin" <<'PYEOF'
import os
import sys

raw = os.environ.get("HOMEBOY_TF_RAW", "")
excerpt = "\n".join(raw.split("\n")[:40]).strip()
if len(excerpt) > 4000:
    excerpt = excerpt[:3997] + "..."
sys.stdout.write(excerpt)
PYEOF
}

homeboy_emit_test_failure() {
    local tf_test_id="" tf_suite="" tf_file="" tf_line="" tf_message="" \
        tf_failure_type="test_failure" tf_identity="" tf_identity_set="0" \
        tf_stdout_excerpt="" tf_stderr_excerpt=""

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --test-id) tf_test_id="${2:-}"; shift 2 ;;
            --suite) tf_suite="${2:-}"; shift 2 ;;
            --file) tf_file="${2:-}"; shift 2 ;;
            --line) tf_line="${2:-}"; shift 2 ;;
            --message) tf_message="${2:-}"; shift 2 ;;
            --failure-type) tf_failure_type="${2:-test_failure}"; shift 2 ;;
            --identity) tf_identity="${2:-}"; tf_identity_set="1"; shift 2 ;;
            --stdout-excerpt) tf_stdout_excerpt="${2:-}"; shift 2 ;;
            --stderr-excerpt) tf_stderr_excerpt="${2:-}"; shift 2 ;;
            *)
                echo "[emit-test-failure] unknown argument: $1" >&2
                return 2
                ;;
        esac
    done

    if [ -z "$tf_test_id" ]; then
        echo "[emit-test-failure] --test-id is required" >&2
        return 2
    fi

    local python_bin
    python_bin="$(homeboy_emit_test_failure_python)" || return 1

    HOMEBOY_TF_TEST_ID="$tf_test_id" \
    HOMEBOY_TF_SUITE="$tf_suite" \
    HOMEBOY_TF_FILE="$tf_file" \
    HOMEBOY_TF_LINE="$tf_line" \
    HOMEBOY_TF_MESSAGE="$tf_message" \
    HOMEBOY_TF_FAILURE_TYPE="$tf_failure_type" \
    HOMEBOY_TF_IDENTITY="$tf_identity" \
    HOMEBOY_TF_IDENTITY_SET="$tf_identity_set" \
    HOMEBOY_TF_STDOUT_EXCERPT="$tf_stdout_excerpt" \
    HOMEBOY_TF_STDERR_EXCERPT="$tf_stderr_excerpt" \
    "$python_bin" <<'PYEOF'
import hashlib
import json
import os
import sys

test_id = os.environ.get("HOMEBOY_TF_TEST_ID", "")
suite = os.environ.get("HOMEBOY_TF_SUITE", "")
file = os.environ.get("HOMEBOY_TF_FILE", "")
line_raw = os.environ.get("HOMEBOY_TF_LINE", "")
message = os.environ.get("HOMEBOY_TF_MESSAGE", "")
failure_type = os.environ.get("HOMEBOY_TF_FAILURE_TYPE", "") or "test_failure"

line = None
if line_raw != "":
    try:
        line = int(line_raw)
    except ValueError:
        line = None

if os.environ.get("HOMEBOY_TF_IDENTITY_SET", "0") == "1":
    identity = os.environ.get("HOMEBOY_TF_IDENTITY", "")
else:
    first_message_line = message.split("\n", 1)[0]
    line_part = str(line) if line is not None else ""
    identity = "\0".join([test_id, file, line_part, failure_type, first_message_line])

record = {
    "test_id": test_id,
    "suite": suite or None,
    "file": file or None,
    "line": line,
    "message": message,
    "failure_type": failure_type,
    "fingerprint": hashlib.sha256(identity.encode("utf-8")).hexdigest(),
    "stdout_excerpt": os.environ.get("HOMEBOY_TF_STDOUT_EXCERPT", ""),
    "stderr_excerpt": os.environ.get("HOMEBOY_TF_STDERR_EXCERPT", ""),
}

json.dump(record, sys.stdout, separators=(",", ":"), ensure_ascii=False)
sys.stdout.write("\n")
PYEOF
}
