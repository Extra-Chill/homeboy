#!/usr/bin/env bash

# Ensure the extension-owned ESLint launcher is backed by its installed package
# tree. Hydrated extension snapshots can retain a .bin symlink without its
# target, so existence alone is not a sufficient readiness check.
homeboy_ensure_eslint_runtime() {
    local extension_path="$1"
    local eslint_bin="${extension_path}/node_modules/.bin/eslint"

    if [ -x "$eslint_bin" ] && "$eslint_bin" --version >/dev/null 2>&1; then
        return 0
    fi

    if ! command -v npm >/dev/null 2>&1; then
        echo "bootstrap failure: npm is required to hydrate the WordPress ESLint runtime" >&2
        return 2
    fi
    if [ ! -f "${extension_path}/package.json" ] || [ ! -f "${extension_path}/package-lock.json" ]; then
        echo "bootstrap failure: WordPress ESLint runtime requires package.json and package-lock.json in ${extension_path}" >&2
        return 2
    fi

    echo "Hydrating WordPress ESLint runtime..." >&2
    if ! (cd "$extension_path" && npm ci --include=dev --no-audit --no-fund); then
        echo "bootstrap failure: could not hydrate the WordPress ESLint runtime" >&2
        return 2
    fi
    if [ ! -x "$eslint_bin" ] || ! "$eslint_bin" --version >/dev/null 2>&1; then
        echo "bootstrap failure: hydrated WordPress ESLint runtime is incomplete" >&2
        return 2
    fi
}
