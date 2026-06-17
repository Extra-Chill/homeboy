#!/usr/bin/env bash

# Bootstrap Homeboy core's shared resolve context helper for direct extension invocation.

__homeboy_runtime_bootstrap_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./runtime-helper-bootstrap.sh
source "${__homeboy_runtime_bootstrap_dir}/runtime-helper-bootstrap.sh"
unset __homeboy_runtime_bootstrap_dir

homeboy_source_core_runtime_helper HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh
