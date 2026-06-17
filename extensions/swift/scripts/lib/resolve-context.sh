#!/usr/bin/env bash

HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-}"
if [ -n "$HELPER" ] && [ "$HELPER" != "${BASH_SOURCE[0]}" ]; then
    # shellcheck source=/dev/null
    source "$HELPER"
else
    COMMON_RESOLVE_CONTEXT_HELPER="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/scripts/lib/resolve-context.sh"
    # shellcheck source=/dev/null
    source "$COMMON_RESOLVE_CONTEXT_HELPER"
fi
