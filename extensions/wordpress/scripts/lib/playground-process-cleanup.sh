#!/usr/bin/env bash

# Scoped cleanup for Playground worker processes spawned during this runner
# invocation. The runner snapshots existing workers before `wp-playground-cli`
# starts, then terminates only new `playground-server-child.mjs` PIDs that are
# still alive when the runner exits or finishes.

homeboy_playground_list_workers() {
    if command -v pgrep >/dev/null 2>&1; then
        pgrep -f 'playground-server-child\.mjs' 2>/dev/null | sort -n || true
        return
    fi

    ps -eo pid=,args= 2>/dev/null | awk '/playground-server-child\.mjs/ && !/awk/ { print $1 }' | sort -n || true
}

homeboy_playground_snapshot_workers() {
    homeboy_playground_list_workers | tr '\n' ' '
}

homeboy_playground_cleanup_new_workers() {
    local before_snapshot="$1"
    local after_pid
    local stale_pids=()

    while IFS= read -r after_pid; do
        [ -z "$after_pid" ] && continue
        case " ${before_snapshot} " in
            *" ${after_pid} "*) ;;
            *) stale_pids+=("$after_pid") ;;
        esac
    done < <(homeboy_playground_list_workers)

    if [ "${#stale_pids[@]}" -eq 0 ]; then
        return 0
    fi

    echo "Cleaning up Playground worker(s) left by this run: ${stale_pids[*]}" >&2
    kill "${stale_pids[@]}" 2>/dev/null || true
    sleep 1

    local remaining=()
    local pid
    for pid in "${stale_pids[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            remaining+=("$pid")
        fi
    done

    if [ "${#remaining[@]}" -gt 0 ]; then
        echo "Force-cleaning Playground worker(s): ${remaining[*]}" >&2
        kill -9 "${remaining[@]}" 2>/dev/null || true
    fi
}
