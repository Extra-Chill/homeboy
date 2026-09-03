#!/usr/bin/env bash

# Verify the extension-owned ESLint launcher is backed by its packaged runtime.
# Runtime installation belongs to extension materialization, not lint execution.
homeboy_ensure_eslint_runtime() {
    local extension_path="$1"
    local eslint_bin="${extension_path}/node_modules/.bin/eslint"

    if [ -x "$eslint_bin" ] && "$eslint_bin" --version >/dev/null 2>&1; then
        return 0
    fi

    echo "bootstrap failure: packaged WordPress ESLint runtime is unavailable at ${eslint_bin}; reinstall or refresh the WordPress extension" >&2
    return 2
}
