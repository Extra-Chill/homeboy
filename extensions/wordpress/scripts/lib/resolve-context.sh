#!/usr/bin/env bash

HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-}"
if [ -n "$HELPER" ] && [ "$HELPER" != "${BASH_SOURCE[0]}" ]; then
    # shellcheck source=/dev/null
    source "$HELPER"
else
    echo "resolve-context wrapper requires HOMEBOY_RUNTIME_RESOLVE_CONTEXT" >&2
    return 2 2>/dev/null || exit 2
fi
