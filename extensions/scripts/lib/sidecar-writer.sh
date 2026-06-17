#!/usr/bin/env bash

# Bootstrap Homeboy core's shared sidecar writer helper for direct extension invocation.

__homeboy_runtime_bootstrap_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./runtime-helper-bootstrap.sh
source "${__homeboy_runtime_bootstrap_dir}/runtime-helper-bootstrap.sh"
unset __homeboy_runtime_bootstrap_dir

homeboy_source_core_runtime_helper HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh
