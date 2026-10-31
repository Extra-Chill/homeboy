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

homeboy_project_adapter_manifest_file() {
    local _ecosystem="$1"
    local _base_dir="${HOMEBOY_DEPENDENCY_ADAPTERS_PATH:-}"
    if [ -z "$_base_dir" ]; then
        _base_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/dependency-adapters/examples"
    fi

    case "$_ecosystem" in
        node|nodejs)
            printf '%s\n' "$_base_dir/nodejs.json"
            ;;
        composer|php)
            printf '%s\n' "$_base_dir/composer.json"
            ;;
        wordpress|wp)
            printf '%s\n' "$_base_dir/wordpress.json"
            ;;
        *)
            return 1
            ;;
    esac
}

homeboy_project_manifest_value() {
    local _manifest="$1"
    local _path="$2"
    MANIFEST_FILE="$_manifest" MANIFEST_PATH="$_path" node -e '
        const fs = require("fs");
        let value = JSON.parse(fs.readFileSync(process.env.MANIFEST_FILE, "utf8"));
        for (const part of process.env.MANIFEST_PATH.split(".")) {
            value = value && value[part];
        }
        if (value === undefined || value === null) process.exit(1);
        if (Array.isArray(value)) console.log(value.join("\n"));
        else console.log(String(value));
    '
}

homeboy_project_find_manifest_root_upward() {
    local _dir="$1"
    local _manifest="$2"
    local _match
    local _files
    _match="$(homeboy_project_manifest_value "$_manifest" "project_signals.root_match" 2>/dev/null || printf 'any')"
    _files="$(homeboy_project_manifest_value "$_manifest" "project_signals.root_files")" || return 1

    while [ "$_dir" != "/" ] && [ "$_dir" != "." ]; do
        local _found=0
        local _missing=0
        while IFS= read -r _file; do
            [ -z "$_file" ] && continue
            if [ -e "$_dir/$_file" ]; then
                _found=1
            else
                _missing=1
            fi
        done <<EOF
$_files
EOF
        if { [ "$_match" = "all" ] && [ "$_missing" -eq 0 ]; } || { [ "$_match" != "all" ] && [ "$_found" -eq 1 ]; }; then
            printf '%s\n' "$_dir"
            return 0
        fi
        _dir="$(dirname "$_dir")"
    done
    return 1
}

homeboy_project_select_package_manager() {
    local _manifest="$1"
    local _root="$2"
    MANIFEST_FILE="$_manifest" PROJECT_ROOT="$_root" node <<'NODE'
const fs = require('fs');
const path = require('path');
const manifest = JSON.parse(fs.readFileSync(process.env.MANIFEST_FILE, 'utf8'));
const root = process.env.PROJECT_ROOT;
const managers = [...(manifest.package_managers || [])].sort((a, b) => a.selection.priority - b.selection.priority);

function findUpward(files) {
    let dir = root;
    while (dir && dir !== path.dirname(dir)) {
        if (files.some((file) => fs.existsSync(path.join(dir, file)))) return dir;
        dir = path.dirname(dir);
    }
    return null;
}

let fallback = null;
for (const manager of managers) {
    const selection = manager.selection || {};
    if (selection.default) fallback = manager;
    const files = selection.files || [];
    const selectedRoot = selection.search === 'upward' ? findUpward(files) : (files.some((file) => fs.existsSync(path.join(root, file))) ? root : null);
    if (selectedRoot) {
        console.log(`${manager.id}\t${selectedRoot}`);
        process.exit(0);
    }
}

if (fallback) {
    console.log(`${fallback.id}\t${root}`);
    process.exit(0);
}

process.exit(1);
NODE
}

homeboy_project_package_manager_value() {
    local _manifest="$1"
    local _manager="$2"
    local _path="$3"
    MANIFEST_FILE="$_manifest" PACKAGE_MANAGER="$_manager" MANAGER_PATH="$_path" node -e '
        const fs = require("fs");
        const manifest = JSON.parse(fs.readFileSync(process.env.MANIFEST_FILE, "utf8"));
        const manager = (manifest.package_managers || []).find((item) => item.id === process.env.PACKAGE_MANAGER);
        let value = manager;
        for (const part of process.env.MANAGER_PATH.split(".")) {
            value = value && value[part];
        }
        if (value === undefined || value === null) process.exit(1);
        console.log(String(value));
    '
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

    if [ -z "$_ecosystem" ]; then
            echo "Error: homeboy_project_init requires --ecosystem" >&2
            return 1
    fi

    homeboy_project_init_manifest "$_ecosystem" "$_dir"
}

homeboy_project_init_manifest() {
    local _ecosystem="$1"
    local _dir="$2"
    local _manifest
    local _root
    local _selection
    local _script_manifest

    if ! _manifest="$(homeboy_project_adapter_manifest_file "$_ecosystem")" || [ ! -f "$_manifest" ]; then
        echo "Error: Unsupported Homeboy project ecosystem: $_ecosystem" >&2
        return 1
    fi

    if ! _root="$(homeboy_project_find_manifest_root_upward "$_dir" "$_manifest")"; then
        echo "Error: No $(homeboy_project_manifest_value "$_manifest" "project_signals.root_files" | tr '\n' '/' | sed 's:/$::') found at or above ${_dir}" >&2
        echo "Not a $(homeboy_project_manifest_value "$_manifest" "ecosystem") project -- cannot run." >&2
        return 1
    fi

    HOMEBOY_PROJECT_ADAPTER_MANIFEST="$_manifest"
    HOMEBOY_PROJECT_ECOSYSTEM="$(homeboy_project_manifest_value "$_manifest" "ecosystem")"
    HOMEBOY_PROJECT_ROOT="$_root"
    HOMEBOY_PROJECT_DEPENDENCY_ROOT="$_root"

    if _selection="$(homeboy_project_select_package_manager "$_manifest" "$_root")"; then
        HOMEBOY_PROJECT_PACKAGE_MANAGER="${_selection%%$'\t'*}"
        HOMEBOY_PROJECT_DEPENDENCY_ROOT="${_selection#*$'\t'}"
        _script_manifest="$(homeboy_project_package_manager_value "$_manifest" "$HOMEBOY_PROJECT_PACKAGE_MANAGER" "scripts.manifest" 2>/dev/null || true)"
        HOMEBOY_PROJECT_RUN_CMD="$(homeboy_project_package_manager_value "$_manifest" "$HOMEBOY_PROJECT_PACKAGE_MANAGER" "scripts.run_command" 2>/dev/null || true)"
        HOMEBOY_PROJECT_EXEC_CMD="$(homeboy_project_package_manager_value "$_manifest" "$HOMEBOY_PROJECT_PACKAGE_MANAGER" "scripts.exec_command" 2>/dev/null || true)"
        if [ -n "$_script_manifest" ]; then
            HOMEBOY_PROJECT_SCRIPT_FILE="${_root}/${_script_manifest}"
        else
            HOMEBOY_PROJECT_SCRIPT_FILE=""
        fi
    else
        HOMEBOY_PROJECT_PACKAGE_MANAGER=""
        HOMEBOY_PROJECT_RUN_CMD=""
        HOMEBOY_PROJECT_EXEC_CMD=""
        HOMEBOY_PROJECT_SCRIPT_FILE=""
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Dependency adapter manifest: $HOMEBOY_PROJECT_ADAPTER_MANIFEST" >&2
        echo "DEBUG: Project ecosystem: $HOMEBOY_PROJECT_ECOSYSTEM" >&2
        echo "DEBUG: Project root: $HOMEBOY_PROJECT_ROOT" >&2
        echo "DEBUG: Dependency root: $HOMEBOY_PROJECT_DEPENDENCY_ROOT" >&2
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
        nodejs|php)
            HOMEBOY_PROJECT_SCRIPT_NAME="$_script" \
            HOMEBOY_PROJECT_SCRIPT_JSON="$HOMEBOY_PROJECT_SCRIPT_FILE" \
                node -e '
                    const pkg = require(process.env.HOMEBOY_PROJECT_SCRIPT_JSON);
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
    local _dir="${HOMEBOY_PROJECT_DEPENDENCY_ROOT:-${HOMEBOY_PROJECT_ROOT:-}}"
    if [ -z "$_dir" ]; then
        echo "Error: homeboy_project_init must be called before dependency helpers" >&2
        return 1
    fi

    if [ "${HOMEBOY_PROJECT_ECOSYSTEM:-}" = "wordpress" ]; then
        local _status=0
        if [ -f "$_dir/composer.json" ]; then
            (homeboy_project_init --ecosystem composer --path "$_dir" && homeboy_project_ensure_dependencies) || _status=$?
        fi
        if [ -f "$_dir/package.json" ]; then
            (homeboy_project_init --ecosystem nodejs --path "$_dir" && homeboy_project_ensure_dependencies) || _status=$?
        fi
        return "$_status"
    fi

    if [ -z "${HOMEBOY_PROJECT_ADAPTER_MANIFEST:-}" ] || [ -z "${HOMEBOY_PROJECT_PACKAGE_MANAGER:-}" ]; then
        echo "Error: Dependency installation is not implemented for ecosystem: ${HOMEBOY_PROJECT_ECOSYSTEM:-}" >&2
        return 1
    fi

    local _output_path
    _output_path="$(homeboy_project_package_manager_value "$HOMEBOY_PROJECT_ADAPTER_MANIFEST" "$HOMEBOY_PROJECT_PACKAGE_MANAGER" "outputs.0.path" 2>/dev/null || true)"
    if [ -n "$_output_path" ] && [ -e "$_dir/$_output_path" ]; then
        return 0
    fi

    local _intent
    local _command
    local _fallback_command
    _intent="$(homeboy_project_package_manager_value "$HOMEBOY_PROJECT_ADAPTER_MANIFEST" "$HOMEBOY_PROJECT_PACKAGE_MANAGER" "install.intent")"
    _command="$(homeboy_project_package_manager_value "$HOMEBOY_PROJECT_ADAPTER_MANIFEST" "$HOMEBOY_PROJECT_PACKAGE_MANAGER" "install.command" 2>/dev/null || true)"
    _fallback_command="$(homeboy_project_package_manager_value "$HOMEBOY_PROJECT_ADAPTER_MANIFEST" "$HOMEBOY_PROJECT_PACKAGE_MANAGER" "install.fallback_command" 2>/dev/null || true)"

    echo "Installing ${HOMEBOY_PROJECT_ECOSYSTEM} dependencies with ${HOMEBOY_PROJECT_PACKAGE_MANAGER}..."
    if [ "$_intent" = "locked-or-refresh" ] && [ -n "$_fallback_command" ]; then
        local _lockfile=""
        case "$HOMEBOY_PROJECT_PACKAGE_MANAGER" in
            npm) _lockfile="package-lock.json" ;;
            composer) _lockfile="composer.lock" ;;
        esac
        if [ -n "$_lockfile" ] \
            && [ -f "$_dir/$_lockfile" ] \
            && command -v git >/dev/null 2>&1 \
            && git -C "$_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
            && git -C "$_dir" ls-files --error-unmatch "$_lockfile" >/dev/null 2>&1; then
            (cd "$_dir" && $_command)
        else
            (cd "$_dir" && $_fallback_command)
        fi
    else
        (cd "$_dir" && $_command)
    fi
}
