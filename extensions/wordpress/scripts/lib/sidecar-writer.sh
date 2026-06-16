#!/usr/bin/env bash

# Direct-invocation fallback wrapper. Homeboy-invoked runners receive the core
# helper via HOMEBOY_RUNTIME_SIDECAR_WRITER; direct runs share one local copy.

# shellcheck source=../../../scripts/lib/sidecar-writer.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib" && pwd)/sidecar-writer.sh"
