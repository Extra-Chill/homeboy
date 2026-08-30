#!/usr/bin/env bash

# Shared step filtering contract for extension runner scripts.
#
# Reads HOMEBOY_STEP / HOMEBOY_SKIP as comma-separated step names and exposes
# should_run_step <name> with the same semantics as Homeboy core's
# RunnerStepFilter:
# - HOMEBOY_STEP present => only listed steps run
# - HOMEBOY_SKIP present => listed steps are skipped
# - empty step name => runs by default

should_run_step() {
    local step_name="${1:-}"
    step_name="$(printf '%s' "$step_name" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"

    if [ -z "$step_name" ]; then
        return 0
    fi

    if [ -n "${HOMEBOY_STEP:-}" ]; then
        case ",${HOMEBOY_STEP}," in
            *",${step_name},"*) ;;
            *) return 1 ;;
        esac
    fi

    if [ -n "${HOMEBOY_SKIP:-}" ]; then
        case ",${HOMEBOY_SKIP}," in
            *",${step_name},"*) return 1 ;;
        esac
    fi

    return 0
}
