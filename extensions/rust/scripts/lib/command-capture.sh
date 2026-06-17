#!/usr/bin/env bash

HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-}"
if [ -n "$HELPER" ] && [ "$HELPER" != "${BASH_SOURCE[0]}" ]; then
    # shellcheck source=/dev/null
    source "$HELPER"
else
    echo "command-capture wrapper requires HOMEBOY_RUNTIME_COMMAND_CAPTURE" >&2
    return 2 2>/dev/null || exit 2
fi
