#!/usr/bin/env bash

HELPER="${HOMEBOY_RUNTIME_SIDECAR_WRITER:-}"
if [ -n "$HELPER" ] && [ "$HELPER" != "${BASH_SOURCE[0]}" ]; then
    # shellcheck source=/dev/null
    source "$HELPER"
else
    echo "sidecar-writer wrapper requires HOMEBOY_RUNTIME_SIDECAR_WRITER" >&2
    return 2 2>/dev/null || exit 2
fi
