#!/usr/bin/env bash

# Shared bash-version preflight for extension scripts that do not need the full
# runner prelude.

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
