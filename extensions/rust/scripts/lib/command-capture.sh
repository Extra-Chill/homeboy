#!/usr/bin/env bash

# Direct-invocation fallback wrapper. Homeboy-invoked runners receive the core
# helper via HOMEBOY_RUNTIME_COMMAND_CAPTURE; direct runs share one local copy.

# shellcheck source=../../../scripts/lib/command-capture.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib" && pwd)/command-capture.sh"
