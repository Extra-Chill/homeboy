#!/usr/bin/env bash
# Parse the Node built-in test runner (`node --test`) summary.
#
# Node prints its run summary in one of two shapes depending on the reporter:
#
#   TAP reporter (`--test-reporter=tap`, or non-TTY output on Node < 23):
#     # tests 42
#     # pass 41
#     # fail 1
#     # skipped 0
#
#   spec reporter (the default on current Node, TTY or not):
#     ℹ tests 42
#     ℹ pass 41
#     ℹ fail 1
#     ℹ cancelled 0
#     ℹ skipped 0
#     ℹ todo 0
#
# Summary lines start at column 0; nested subtest output is indented, so only
# unindented lines are read. A package script that runs `node --test` more
# than once prints one summary per process, and the counts are summed.
#
# Cancelled tests make `node --test` exit non-zero, so they count as failures.
# Todo tests count as skipped.
#
# Usage: homeboy_node_test_parse_summary <output>
# Returns 0 and sets NODE_TEST_TOTAL, NODE_TEST_PASSED, NODE_TEST_FAILED and
# NODE_TEST_SKIPPED when a summary is present; returns 1 otherwise.

homeboy_node_test_summary_sum() {
    local summary="$1" key="$2"
    printf '%s\n' "$summary" | awk -v key="$key" '
        $1 == key && $2 ~ /^[0-9]+$/ { sum += $2 }
        END { print sum + 0 }
    '
}

homeboy_node_test_parse_summary() {
    local output="$1" summary

    # The caller strips ANSI colour codes. Normalize both reporter prefixes to
    # bare "<key> <count>" lines.
    summary="$(printf '%s\n' "$output" \
        | sed -nE 's/^(# |ℹ )(tests|pass|fail|cancelled|skipped|todo)[[:space:]]+([0-9]+)[[:space:]]*$/\2 \3/p')"

    if ! printf '%s\n' "$summary" | grep -qE '^tests [0-9]+$'; then
        return 1
    fi

    NODE_TEST_TOTAL="$(homeboy_node_test_summary_sum "$summary" tests)"
    NODE_TEST_PASSED="$(homeboy_node_test_summary_sum "$summary" pass)"
    NODE_TEST_FAILED=$(( $(homeboy_node_test_summary_sum "$summary" fail) + $(homeboy_node_test_summary_sum "$summary" cancelled) ))
    NODE_TEST_SKIPPED=$(( $(homeboy_node_test_summary_sum "$summary" skipped) + $(homeboy_node_test_summary_sum "$summary" todo) ))
    return 0
}
