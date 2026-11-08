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
#
# Record shaping runs in the running Homeboy binary, exported to runners as
# HOMEBOY_RUNTIME_EXECUTABLE; no interpreter is required.

homeboy_emit_test_failure_runtime_json() {
    if [ -z "${HOMEBOY_RUNTIME_EXECUTABLE:-}" ]; then
        echo "[emit-test-failure] HOMEBOY_RUNTIME_EXECUTABLE is required" >&2
        return 1
    fi
    "$HOMEBOY_RUNTIME_EXECUTABLE" __homeboy_runtime_json "$@"
}

# Bound a raw output blob to the canonical sidecar excerpt shape: keep the first
# 40 lines, trim surrounding whitespace, and cap the result at 4000 characters
# (replacing the tail with "..."). Mirrors WordPress make_output_excerpt.
homeboy_test_failure_bound_excerpt() {
    printf '%s' "${1:-}" | homeboy_emit_test_failure_runtime_json test-failure-excerpt
}

homeboy_emit_test_failure() {
    homeboy_emit_test_failure_runtime_json test-failure "$@"
}
