#!/usr/bin/env bash

COMMON_RUNNER_STEPS_HELPER="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/scripts/lib/runner-steps.sh"
# shellcheck source=/dev/null
source "$COMMON_RUNNER_STEPS_HELPER"
