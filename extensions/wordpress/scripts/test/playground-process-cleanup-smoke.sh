#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER="${SCRIPT_DIR}/../lib/playground-process-cleanup.sh"

# shellcheck source=../lib/playground-process-cleanup.sh
source "$HELPER"

spawn_fake_worker() {
    bash -c 'exec -a playground-server-child.mjs sleep 60' &
    FAKE_WORKER_PID=$!
}

cleanup_pids=()
cleanup() {
    local pid
    for pid in "${cleanup_pids[@]:-}"; do
        kill "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

spawn_fake_worker
protected_pid=$FAKE_WORKER_PID
cleanup_pids+=("$protected_pid")
sleep 0.2

before_snapshot=$(homeboy_playground_snapshot_workers)
case " ${before_snapshot} " in
    *" ${protected_pid} "*) ;;
    *)
        echo "Expected protected fake worker to appear in snapshot" >&2
        echo "Snapshot: ${before_snapshot}" >&2
        exit 1
        ;;
esac

spawn_fake_worker
new_pid=$FAKE_WORKER_PID
cleanup_pids+=("$new_pid")
sleep 0.2

homeboy_playground_cleanup_new_workers "$before_snapshot" >/tmp/homeboy-playground-cleanup-smoke.out 2>&1
sleep 0.2

if kill -0 "$new_pid" 2>/dev/null; then
    echo "Expected cleanup to terminate new fake worker $new_pid" >&2
    cat /tmp/homeboy-playground-cleanup-smoke.out >&2
    exit 1
fi

if ! kill -0 "$protected_pid" 2>/dev/null; then
    echo "Expected cleanup to preserve pre-existing fake worker $protected_pid" >&2
    cat /tmp/homeboy-playground-cleanup-smoke.out >&2
    exit 1
fi

if ! grep -Fq "Cleaning up Playground worker(s) left by this run" /tmp/homeboy-playground-cleanup-smoke.out; then
    echo "Expected cleanup diagnostic" >&2
    cat /tmp/homeboy-playground-cleanup-smoke.out >&2
    exit 1
fi

echo "Playground process cleanup smoke passed"
