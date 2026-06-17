#!/usr/bin/env bash

HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-}"
if [ -n "$HELPER" ] && [ "$HELPER" != "${BASH_SOURCE[0]}" ]; then
    # shellcheck source=/dev/null
    source "$HELPER"
else
    echo "runner-steps wrapper requires HOMEBOY_RUNTIME_RUNNER_STEPS" >&2
    return 2 2>/dev/null || exit 2
fi
