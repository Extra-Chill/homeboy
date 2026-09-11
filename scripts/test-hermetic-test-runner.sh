#!/bin/sh
# Focused regression tests for the globally configured Cargo test runner.
# Each invocation has an independent external alarm: this test must remain
# bounded even if the runner regresses.
set -eu

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
runner="$root/scripts/nextest-hermetic-test-environment.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-hermetic-runner-test.XXXXXX")"

cleanup_known_descendants() {
    for pid_file in "$scratch"/*descendant-pid; do
        [ -f "$pid_file" ] || continue
        IFS="$(printf '\t')" read -r pid started < "$pid_file" || continue
        [ -n "$started" ] || continue
        current="$(/bin/ps -p "$pid" -o lstart= 2>/dev/null || true)"
        [ "$current" = "$started" ] || continue
        kill -TERM "$pid" 2>/dev/null || true
        sleep 1
        current="$(/bin/ps -p "$pid" -o lstart= 2>/dev/null || true)"
        [ "$current" = "$started" ] && kill -KILL "$pid" 2>/dev/null || true
    done
}

trap 'cleanup_known_descendants; rm -rf "$scratch"' EXIT HUP INT TERM

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
HOMEBOY_TEST_TIMEOUT_SECONDS=1 run_bounded sh -c 'printf "%s" "$TMPDIR" > "$1"; trap "" TERM; (trap "" TERM; while :; do sleep 1; done) & pid=$!; printf "%s\t" "$pid" > "$2"; /bin/ps -p "$pid" -o lstart= >> "$2"; while :; do sleep 1; done' sh "$scratch/hung-tmp" "$scratch/descendant-pid" >"$scratch/hung.stdout" 2>"$scratch/hung.stderr"
status=$?
set -e
[ "$status" -eq 124 ]
hung_tmp="$(<"$scratch/hung-tmp")"
[ ! -e "$hung_tmp" ]
IFS="$(printf '\t')" read -r descendant _ < "$scratch/descendant-pid"
if kill -0 "$descendant" 2>/dev/null; then
    echo "TERM-resistant descendant survived runner timeout: $descendant" >&2
    exit 1
fi
if ! perl -0777 -ne 'exit(/test binary .* exceeded suite deadline 1s/ ? 0 : 1)' "$scratch/hung.stderr"; then
    echo "timeout diagnostic did not name the binary and deadline" >&2
    exit 1
fi

run_separate_session_fixture() {
    output_prefix="$1"
    timeout_seconds="$2"
    HOMEBOY_TEST_TIMEOUT_SECONDS="$timeout_seconds" perl -e 'alarm shift @ARGV; exec @ARGV or die "exec: $!\n"' 8 "$runner" sh -c '
        printf "%s" "$TMPDIR" > "$1"
        perl -MPOSIX=setsid -e "setsid() or die qq(setsid: \$!\\n); \$SIG{TERM} = q(IGNORE); \$SIG{INT} = q(IGNORE); open my \$pid, q(>), \$ARGV[0] or die qq(pid: \$!\\n); print \$pid qq(\$\$\\t), qx(/bin/ps -p \$\$ -o lstart=); close \$pid; while (1) { sleep 1 }" "$2" &
        while :; do sleep 1; done
    ' sh "$scratch/$output_prefix-tmp" "$scratch/$output_prefix-descendant-pid" >"$scratch/$output_prefix.stdout" 2>"$scratch/$output_prefix.stderr" &
    runner_pid=$!
    deadline=$(( $(date +%s) + 5 ))
    while [ ! -f "$scratch/$output_prefix-descendant-pid" ] && [ "$(date +%s)" -lt "$deadline" ]; do
        sleep 1
    done
    if [ ! -f "$scratch/$output_prefix-descendant-pid" ]; then
        kill -KILL "$runner_pid" 2>/dev/null || true
        wait "$runner_pid" 2>/dev/null || true
        echo "separate-session fixture did not publish its PID" >&2
        exit 1
    fi
}

assert_separate_session_reaped() {
    output_prefix="$1"
    expected_status="$2"
    set +e
    wait "$runner_pid"
    status=$?
    set -e
    [ "$status" -eq "$expected_status" ]
    tmp="$(<"$scratch/$output_prefix-tmp")"
    [ ! -e "$tmp" ]
    IFS="$(printf '\t')" read -r descendant _ < "$scratch/$output_prefix-descendant-pid"
    if kill -0 "$descendant" 2>/dev/null; then
        echo "separate-session TERM-resistant descendant survived: $descendant" >&2
        exit 1
    fi
}

run_separate_session_fixture timeout 1
sleep 2
assert_separate_session_reaped timeout 124
if ! perl -0777 -ne 'exit(/test binary .* exceeded suite deadline 1s/ ? 0 : 1)' "$scratch/timeout.stderr"; then
    echo "timeout cleanup diagnostic did not name the binary and deadline" >&2
    exit 1
fi

for signal in TERM INT; do
    run_separate_session_fixture "$signal" 30
    kill -"$signal" "$runner_pid"
    assert_separate_session_reaped "$signal" 143
done

set +e
HOMEBOY_TEST_TIMEOUT_SECONDS=0 run_bounded sh -c 'exit 0' >"$scratch/zero.stdout" 2>"$scratch/zero.stderr"
status=$?
set -e
[ "$status" -eq 2 ]
if ! perl -0777 -ne 'exit(/must be a positive integer/ ? 0 : 1)' "$scratch/zero.stderr"; then
    echo "zero timeout was not rejected" >&2
    exit 1
fi
