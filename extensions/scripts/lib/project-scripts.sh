#!/usr/bin/env bash

# Generic project script helpers for extension runners.
#
# Extensions select an ecosystem adapter with homeboy_project_init, then use the
# project helpers instead of hand-rolling package-manager or script checks.

homeboy_project_find_file_upward() {
    local _dir="$1"
    local _file="$2"
    while [ "$_dir" != "/" ] && [ "$_dir" != "." ]; do
        if [ -f "$_dir/$_file" ]; then
            printf '%s\n' "$_dir"
            return 0
        fi
        _dir="$(dirname "$_dir")"
    done
    return 1
}

homeboy_project_init() {
    local _ecosystem=""
    local _dir="${PROJECT_PATH:-.}"

    while [ $# -gt 0 ]; do
        case "$1" in
            --ecosystem)
                _ecosystem="${2:-}"
                shift 2
                ;;
            --path)
                _dir="${2:-}"
                shift 2
                ;;
            *)
                echo "Error: Unknown homeboy_project_init argument: $1" >&2
                return 1
                ;;
        esac
    done

    case "$_ecosystem" in
        node|nodejs)
            homeboy_project_init_node "$_dir"
            ;;
        "")
            echo "Error: homeboy_project_init requires --ecosystem" >&2
            return 1
            ;;
        *)
            echo "Error: Unsupported Homeboy project ecosystem: $_ecosystem" >&2
            return 1
            ;;
    esac
}

homeboy_project_init_node() {
    local _dir="$1"
    local _root

    if ! _root="$(homeboy_project_find_file_upward "$_dir" "package.json")"; then
        echo "Error: No package.json found at or above ${_dir}" >&2
        echo "Not a Node.js project -- cannot run." >&2
        return 1
    fi

    HOMEBOY_PROJECT_ECOSYSTEM="node"
    HOMEBOY_PROJECT_ROOT="$_root"
    HOMEBOY_PROJECT_SCRIPT_FILE="${_root}/package.json"

    if [ -f "$_root/pnpm-lock.yaml" ] && command -v pnpm >/dev/null 2>&1; then
        HOMEBOY_PROJECT_PACKAGE_MANAGER="pnpm"
        HOMEBOY_PROJECT_RUN_CMD="pnpm run"
        HOMEBOY_PROJECT_EXEC_CMD="pnpm exec"
    elif [ -f "$_root/yarn.lock" ] && command -v yarn >/dev/null 2>&1; then
        HOMEBOY_PROJECT_PACKAGE_MANAGER="yarn"
        HOMEBOY_PROJECT_RUN_CMD="yarn"
        HOMEBOY_PROJECT_EXEC_CMD="yarn"
    else
        HOMEBOY_PROJECT_PACKAGE_MANAGER="npm"
        HOMEBOY_PROJECT_RUN_CMD="npm run"
        HOMEBOY_PROJECT_EXEC_CMD="npx"
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Project ecosystem: $HOMEBOY_PROJECT_ECOSYSTEM" >&2
        echo "DEBUG: Project root: $HOMEBOY_PROJECT_ROOT" >&2
        echo "DEBUG: Package manager: $HOMEBOY_PROJECT_PACKAGE_MANAGER" >&2
    fi
}

homeboy_project_require_script_file() {
    if [ -z "${HOMEBOY_PROJECT_SCRIPT_FILE:-}" ] || [ ! -f "$HOMEBOY_PROJECT_SCRIPT_FILE" ]; then
        echo "Error: homeboy_project_init must be called before project script helpers" >&2
        return 1
    fi
}

homeboy_project_has_script() {
    local _script="$1"
    homeboy_project_require_script_file || return 1

    case "${HOMEBOY_PROJECT_ECOSYSTEM:-}" in
        node)
            HOMEBOY_PROJECT_SCRIPT_NAME="$_script" \
            HOMEBOY_PROJECT_PACKAGE_JSON="$HOMEBOY_PROJECT_SCRIPT_FILE" \
                node -e '
                    const pkg = require(process.env.HOMEBOY_PROJECT_PACKAGE_JSON);
                    const script = process.env.HOMEBOY_PROJECT_SCRIPT_NAME;
                    process.exit(pkg.scripts && pkg.scripts[script] ? 0 : 1);
                ' 2>/dev/null
            ;;
        *)
            echo "Error: Unsupported Homeboy project ecosystem: ${HOMEBOY_PROJECT_ECOSYSTEM:-}" >&2
            return 1
            ;;
    esac
}

homeboy_project_run_script_command() {
    local _script="$1"
    if [ -z "${HOMEBOY_PROJECT_RUN_CMD:-}" ]; then
        echo "Error: homeboy_project_init must be called before project script helpers" >&2
        return 1
    fi
    printf '%s %s' "$HOMEBOY_PROJECT_RUN_CMD" "$_script"
}

homeboy_project_exec_command() {
    local _binary="$1"
    shift || true
    if [ -z "${HOMEBOY_PROJECT_EXEC_CMD:-}" ]; then
        echo "Error: homeboy_project_init must be called before project script helpers" >&2
        return 1
    fi
    printf '%s %s' "$HOMEBOY_PROJECT_EXEC_CMD" "$_binary"
    if [ $# -gt 0 ]; then
        printf ' %s' "$@"
    fi
}

homeboy_project_run_script() {
    local _script="$1"
    shift || true
    homeboy_project_require_script_file || return 1
    (cd "$HOMEBOY_PROJECT_ROOT" && $(homeboy_project_run_script_command "$_script") "$@")
}

homeboy_project_exec() {
    local _binary="$1"
    shift || true
    if [ -z "${HOMEBOY_PROJECT_ROOT:-}" ]; then
        echo "Error: homeboy_project_init must be called before project script helpers" >&2
        return 1
    fi
    (cd "$HOMEBOY_PROJECT_ROOT" && $HOMEBOY_PROJECT_EXEC_CMD "$_binary" "$@")
}

homeboy_project_ensure_dependencies() {
    if [ "${HOMEBOY_PROJECT_ECOSYSTEM:-}" != "node" ]; then
        echo "Error: Dependency installation is not implemented for ecosystem: ${HOMEBOY_PROJECT_ECOSYSTEM:-}" >&2
        return 1
    fi

    local _dir="${HOMEBOY_PROJECT_ROOT:-}"
    if [ -z "$_dir" ]; then
        echo "Error: homeboy_project_init must be called before dependency helpers" >&2
        return 1
    fi

    if [ -d "$_dir/node_modules" ]; then
        return 0
    fi

    echo "Installing Node.js dependencies..."
    case "${HOMEBOY_PROJECT_PACKAGE_MANAGER:-npm}" in
        pnpm)
            (cd "$_dir" && pnpm install --frozen-lockfile)
            ;;
        yarn)
            (cd "$_dir" && yarn install --frozen-lockfile)
            ;;
        npm|*)
            if [ -f "$_dir/package-lock.json" ]; then
                (cd "$_dir" && npm ci)
            else
                (cd "$_dir" && npm install)
            fi
            ;;
    esac
}
