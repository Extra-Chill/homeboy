#!/usr/bin/env bash

# Shared step filtering for extension runners.
#
# HOMEBOY_STEP is a comma-separated allowlist. When set, only listed steps run.
# HOMEBOY_SKIP is a comma-separated denylist. Denylist wins when both are set.

homeboy_step_list_contains() {
    local list="$1"
    local needle="$2"
    local item

    IFS=',' read -ra _homeboy_step_items <<< "$list"
    for item in "${_homeboy_step_items[@]}"; do
        item="${item//[[:space:]]/}"
        if [ "$item" = "$needle" ]; then
            return 0
        fi
    done

    return 1
}

should_run_step() {
    local step="$1"

    if [ -n "${HOMEBOY_SKIP:-}" ] && homeboy_step_list_contains "${HOMEBOY_SKIP}" "$step"; then
        return 1
    fi

    if [ -n "${HOMEBOY_STEP:-}" ]; then
        homeboy_step_list_contains "${HOMEBOY_STEP}" "$step"
        return $?
    fi

    return 0
}
