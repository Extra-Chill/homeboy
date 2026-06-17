#!/usr/bin/env bash

HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-}"
if [ -n "$HELPER" ] && [ "$HELPER" != "${BASH_SOURCE[0]}" ]; then
    # shellcheck source=/dev/null
    source "$HELPER"
else
    COMMON_RUNNER_STEPS_HELPER="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/scripts/lib/runner-steps.sh"
    # shellcheck source=/dev/null
    source "$COMMON_RUNNER_STEPS_HELPER"
fi
