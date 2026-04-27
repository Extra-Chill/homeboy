#!/usr/bin/env bash

COMMON_RESOLVE_CONTEXT_HELPER="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/scripts/lib/resolve-context.sh"
# shellcheck source=/dev/null
source "$COMMON_RESOLVE_CONTEXT_HELPER"
