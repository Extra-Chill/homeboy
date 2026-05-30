#!/usr/bin/env bash

# Shared bootstrap for shell-based Homeboy extension runners.
#
# Runner scripts source this file, then call homeboy_runner_init with the
# runtime pieces they need:
#   homeboy_runner_init --bash 4 --component-alias PLUGIN_PATH --steps --failure-trap

homeboy_runtime_dir() {
    cd "$(dirname "${BASH_SOURCE[0]}")" && pwd
}

homeboy_require_bash_version() {
    local required_major="$1"

    if [ -z "${BASH_VERSION:-}" ] || ((BASH_VERSINFO[0] < required_major)); then
        echo "ERROR: bash ${required_major}.0+ required (found ${BASH_VERSION:-non-bash shell})" >&2
        case "$(uname -s)" in
            Darwin)
                echo "macOS ships with bash 3.2. Install newer bash: brew install bash" >&2
                echo "Then restart your terminal so Homebrew bash takes priority on PATH." >&2
                ;;
            Linux)
                echo "Update bash via your package manager (apt, dnf, pacman, etc.)" >&2
                ;;
            MINGW*|MSYS*|CYGWIN*)
                echo "Update Git Bash or use WSL with a modern bash version" >&2
                ;;
            *)
                echo "Install bash ${required_major}.0 or later for your platform" >&2
                ;;
        esac
        exit 1
    fi
}

homeboy_source_runtime_helper() {
    local env_var="$1"
    local fallback="$2"
    local required="${3:-required}"
    local helper="${!env_var:-}"

    if [ -z "$helper" ]; then
        helper="$fallback"
    fi

    if [ -n "$helper" ] && [ -f "$helper" ]; then
        # shellcheck source=/dev/null
        source "$helper"
        return 0
    fi

    if [ "$required" = "optional" ]; then
        return 0
    fi

    echo "homeboy_runner_init: missing runtime helper for ${env_var}: ${helper}" >&2
    return 2
}

homeboy_runner_init() {
    local required_bash=""
    local project_alias="PROJECT_PATH"
    local component_alias=""
    local load_steps=0
    local load_failure_trap=0
    local load_sidecar_writer=0
    local runtime_dir
    runtime_dir="$(homeboy_runtime_dir)"

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --bash)
                required_bash="${2:-}"
                shift 2
                ;;
            --project-alias)
                project_alias="${2:-}"
                shift 2
                ;;
            --component-alias)
                component_alias="${2:-}"
                shift 2
                ;;
            --steps)
                load_steps=1
                shift
                ;;
            --failure-trap)
                load_failure_trap=1
                shift
                ;;
            --sidecar-writer)
                load_sidecar_writer=1
                shift
                ;;
            *)
                echo "homeboy_runner_init: unknown argument: $1" >&2
                return 2
                ;;
        esac
    done

    if [ -n "$required_bash" ]; then
        homeboy_require_bash_version "$required_bash"
    fi

    homeboy_source_runtime_helper HOMEBOY_RUNTIME_RESOLVE_CONTEXT "${runtime_dir}/resolve-context.sh"
    homeboy_resolve_context --project-alias "$project_alias" --component-alias "$component_alias"

    if [ "$load_steps" -eq 1 ]; then
        homeboy_source_runtime_helper HOMEBOY_RUNTIME_RUNNER_STEPS "${runtime_dir}/runner-steps.sh"
    fi

    if [ "$load_sidecar_writer" -eq 1 ]; then
        homeboy_source_runtime_helper HOMEBOY_RUNTIME_SIDECAR_WRITER "${runtime_dir}/sidecar-writer.sh" optional
    fi

    if [ "$load_failure_trap" -eq 1 ]; then
        if homeboy_source_runtime_helper HOMEBOY_RUNTIME_FAILURE_TRAP "${runtime_dir}/failure-trap.sh" optional; then
            if type homeboy_init_failure_trap >/dev/null 2>&1; then
                homeboy_init_failure_trap
            else
                FAILED_STEP=""
                FAILURE_OUTPUT=""
                FAILURE_REPLAY_MODE="full"
            fi
        fi
    fi
}
