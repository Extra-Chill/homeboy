#!/bin/sh
# Focused regression tests for the globally configured Cargo test runner.
# Each invocation has an independent external alarm: this test must remain
# bounded even if the runner regresses.
set -eu

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
runner="$root/scripts/nextest-hermetic-test-environment.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-hermetic-runner-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT HUP INT TERM

run_bounded() {
    perl -e 'alarm shift @ARGV; exec @ARGV or die "exec: $!\n"' 8 "$runner" "$@"
}

started="$(date +%s)"
run_bounded sh -c 'printf "%s" "$TMPDIR" > "$1"; exit 0' sh "$scratch/clean-tmp"
elapsed=$(( $(date +%s) - started ))
[ "$elapsed" -lt 3 ]
clean_tmp="$(<"$scratch/clean-tmp")"
[ ! -e "$clean_tmp" ]

set +e
HOMEBOY_TEST_TIMEOUT_SECONDS=1 run_bounded sh -c 'printf "%s" "$TMPDIR" > "$1"; trap "" TERM; (trap "" TERM; while :; do sleep 1; done) & printf "%s" "$!" > "$2"; while :; do sleep 1; done' sh "$scratch/hung-tmp" "$scratch/descendant-pid" >"$scratch/hung.stdout" 2>"$scratch/hung.stderr"
status=$?
set -e
[ "$status" -eq 124 ]
hung_tmp="$(<"$scratch/hung-tmp")"
[ ! -e "$hung_tmp" ]
descendant="$(<"$scratch/descendant-pid")"
if kill -0 "$descendant" 2>/dev/null; then
    echo "TERM-resistant descendant survived runner timeout: $descendant" >&2
    exit 1
fi
if ! perl -0777 -ne 'exit(/test binary .* exceeded suite deadline 1s/ ? 0 : 1)' "$scratch/hung.stderr"; then
    echo "timeout diagnostic did not name the binary and deadline" >&2
    exit 1
fi

set +e
HOMEBOY_TEST_TIMEOUT_SECONDS=0 run_bounded sh -c 'exit 0' >"$scratch/zero.stdout" 2>"$scratch/zero.stderr"
status=$?
set -e
[ "$status" -eq 2 ]
if ! perl -0777 -ne 'exit(/must be a positive integer/ ? 0 : 1)' "$scratch/zero.stderr"; then
    echo "zero timeout was not rejected" >&2
    exit 1
fi
