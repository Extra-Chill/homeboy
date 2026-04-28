#!/usr/bin/env bash

# Filter the known @wp-playground/cli stale-temp cleanup race where a temp path
# disappears after discovery but before the CLI can lstat it. Real cleanup
# failures still pass through so release gates keep useful diagnostics.
homeboy_filter_playground_cleanup_noise() {
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        cat
        return
    fi

    awk '
        /^Failed to find stale Playground temp dirs: Error: ENOENT: no such file or directory, lstat / { next }
        { print }
    '
}
