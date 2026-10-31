#!/usr/bin/env bash

# PHP version preflight check.
#
# Compares the runtime PHP version against constraints in a component's
# composer.json (require.php and require-dev package constraints like
# phpunit/phpunit that imply a minimum PHP version).
#
# Usage: source this file, then call:
#   homeboy_php_preflight <component_path>
#
# Returns 0 if OK, exits 1 with diagnostic if incompatible.

# Known packages that impose PHP version floors.
# Format: "package_name:constraint_prefix:min_php"
# These are checked when the direct `require.php` constraint is absent or loose.
_KNOWN_PHP_FLOORS=(
    "phpunit/phpunit:^12:8.3"
    "phpunit/phpunit:^11:8.2"
    "phpunit/phpunit:^10:8.1"
)

# Extract minimum PHP version from a composer version constraint.
# Handles: ">=8.2", "^8.1", "~8.0", "8.2.*", ">=8.2 <9", "8.2|8.3"
_extract_min_php_version() {
    local constraint="$1"
    echo "$constraint" | php -r '
        $c = trim(file_get_contents("php://stdin"));
        if ($c === "") exit;
        // Extract first version-like pattern
        if (preg_match("/(\d+\.\d+)/", $c, $m)) {
            echo $m[1];
        }
    ' 2>/dev/null || echo ""
}

# Compare two version strings (major.minor).
# Returns: 0 if $1 >= $2, 1 otherwise.
_version_gte() {
    local current="$1"
    local required="$2"

    local cur_major cur_minor req_major req_minor
    cur_major=$(echo "$current" | cut -d. -f1)
    cur_minor=$(echo "$current" | cut -d. -f2)
    req_major=$(echo "$required" | cut -d. -f1)
    req_minor=$(echo "$required" | cut -d. -f2)

    if [ "$cur_major" -gt "$req_major" ] 2>/dev/null; then
        return 0
    elif [ "$cur_major" -eq "$req_major" ] 2>/dev/null && [ "$cur_minor" -ge "$req_minor" ] 2>/dev/null; then
        return 0
    fi
    return 1
}

# Main preflight function.
# Checks the runtime PHP version against composer.json constraints.
homeboy_php_preflight() {
    local component_path="${1:-}"

    if [ -z "$component_path" ]; then
        return 0
    fi

    local composer_file="${component_path}/composer.json"
    if [ ! -f "$composer_file" ]; then
        return 0
    fi

    if ! command -v php &>/dev/null; then
        echo "Warning: PHP not found, skipping version preflight"
        return 0
    fi

    # Get runtime PHP version (major.minor)
    local runtime_version
    runtime_version=$(php -r 'echo PHP_MAJOR_VERSION . "." . PHP_MINOR_VERSION;' 2>/dev/null)
    if [ -z "$runtime_version" ]; then
        return 0
    fi

    # Check direct require.php constraint
    local php_constraint
    php_constraint=$(php -r '
        $json = json_decode(file_get_contents($argv[1]), true);
        echo $json["require"]["php"] ?? "";
    ' "$composer_file" 2>/dev/null || echo "")

    if [ -n "$php_constraint" ]; then
        local min_required
        min_required=$(_extract_min_php_version "$php_constraint")
        if [ -n "$min_required" ]; then
            if ! _version_gte "$runtime_version" "$min_required"; then
                echo "" >&2
                echo "============================================" >&2
                echo "ERROR: PHP version mismatch" >&2
                echo "============================================" >&2
                echo "  Runtime PHP:    ${runtime_version}" >&2
                echo "  Required:       ${php_constraint} (minimum ${min_required})" >&2
                echo "  Source:         ${composer_file} → require.php" >&2
                echo "" >&2
                echo "  Fix: Use PHP ${min_required}+ or adjust the constraint in composer.json" >&2
                echo "" >&2
                exit 1
            fi
        fi
    fi

    # Check known packages that impose PHP version floors
    for entry in "${_KNOWN_PHP_FLOORS[@]}"; do
        local pkg constraint_prefix min_php
        pkg=$(echo "$entry" | cut -d: -f1)
        constraint_prefix=$(echo "$entry" | cut -d: -f2)
        min_php=$(echo "$entry" | cut -d: -f3)

        # Check both require and require-dev
        local pkg_constraint
        pkg_constraint=$(php -r '
            $json = json_decode(file_get_contents($argv[1]), true);
            $pkg = $argv[2];
            echo $json["require"][$pkg] ?? $json["require-dev"][$pkg] ?? "";
        ' "$composer_file" "$pkg" 2>/dev/null || echo "")

        if [ -z "$pkg_constraint" ]; then
            continue
        fi

        # Check if the constraint starts with our known prefix
        case "$pkg_constraint" in
            ${constraint_prefix}*)
                if ! _version_gte "$runtime_version" "$min_php"; then
                    echo "" >&2
                    echo "============================================" >&2
                    echo "ERROR: PHP version mismatch (dependency)" >&2
                    echo "============================================" >&2
                    echo "  Runtime PHP:    ${runtime_version}" >&2
                    echo "  Package:        ${pkg}:${pkg_constraint}" >&2
                    echo "  Requires:       PHP >= ${min_php}" >&2
                    echo "  Source:         ${composer_file}" >&2
                    echo "" >&2
                    echo "  Fix: Use PHP ${min_php}+ or downgrade ${pkg} in composer.json" >&2
                    echo "" >&2
                    exit 1
                fi
                ;;
        esac
    done

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: PHP preflight passed (runtime ${runtime_version})"
    fi

    return 0
}
