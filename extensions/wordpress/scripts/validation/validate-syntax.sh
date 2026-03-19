#!/usr/bin/env bash
#
# Post-write PHP syntax validation for homeboy's validate_write gate.
#
# Runs `php -l` on all PHP source files in the project root, excluding
# vendor/, node_modules/, and common non-source directories.
#
# Called by homeboy after `audit --fix --write` or `refactor` applies changes.
# Runs from the project root (CWD = component source path).
#
# Exit 0 = all files pass syntax check
# Exit 1 = at least one syntax error found (homeboy will rollback)

set -uo pipefail

ROOT="${PWD}"
errors=0
error_output=""

while IFS= read -r -d '' php_file; do
    if result=$(php -l "$php_file" 2>&1); then
        : # syntax OK
    else
        errors=$((errors + 1))
        error_output+="${result}"$'\n'
    fi
done < <(find "$ROOT" \
    -name '*.php' \
    -not -path '*/vendor/*' \
    -not -path '*/node_modules/*' \
    -not -path '*/.git/*' \
    -print0)

if [ "$errors" -gt 0 ]; then
    echo "PHP syntax validation failed: ${errors} file(s) with errors" >&2
    echo "$error_output" >&2
    exit 1
fi

echo "PHP syntax OK"
exit 0
