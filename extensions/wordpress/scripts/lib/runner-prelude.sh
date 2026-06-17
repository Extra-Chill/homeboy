#!/usr/bin/env bash

# Direct-invocation fallback wrapper. Prefer the core runtime helper when the
# caller provided it; direct runs without Homeboy still share one local copy.

HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-}"
if [ -n "$HELPER" ] && [ "$HELPER" != "${BASH_SOURCE[0]}" ]; then
    # shellcheck source=/dev/null
    source "$HELPER"
else
    # shellcheck source=../../../scripts/lib/runner-prelude.sh
    source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib" && pwd)/runner-prelude.sh"
fi
