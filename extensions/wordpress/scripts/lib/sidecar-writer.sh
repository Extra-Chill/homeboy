#!/usr/bin/env bash

# Direct-invocation fallback wrapper. Prefer the core runtime helper when the
# caller provided it; direct runs without Homeboy still share one local copy.

HELPER="${HOMEBOY_RUNTIME_SIDECAR_WRITER:-}"
if [ -n "$HELPER" ] && [ "$HELPER" != "${BASH_SOURCE[0]}" ]; then
    # shellcheck source=/dev/null
    source "$HELPER"
else
    # shellcheck source=../../../scripts/lib/sidecar-writer.sh
    source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib" && pwd)/sidecar-writer.sh"
fi
