#!/usr/bin/env bash

# Direct-invocation fallback wrapper. Homeboy-invoked runners receive the core
# helper via HOMEBOY_RUNTIME_RUNNER_PRELUDE; direct runs share one local copy.

# shellcheck source=../../../scripts/lib/runner-prelude.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib" && pwd)/runner-prelude.sh"
