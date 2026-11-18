#!/usr/bin/env bash

# Shared install bridge for extension dependency runners and prepare steps.
# Keep build/pack/install logic in the local-workspace-deps.sh helper; an empty
# declaration needs no sibling.

# Succeeds when HOMEBOY_SETTINGS_JSON declares a non-empty (or malformed, so the
# helper reports it) local_workspace_dependencies value.
homeboy_local_workspace_dependencies_declared() {
    case "${HOMEBOY_SETTINGS_JSON:-}" in
        ""|"{}") return 1 ;;
    esac
    if node -e '
        try {
            const entries = JSON.parse(process.argv[1]).local_workspace_dependencies;
            process.exit(entries === undefined || (Array.isArray(entries) && entries.length === 0) ? 0 : 1);
        } catch { process.exit(1); }
    ' "$HOMEBOY_SETTINGS_JSON"; then
        return 1
    fi
    return 0
}

homeboy_apply_local_workspace_install() {
    local consumer_dir="$1"
    local collection_dir="$2"
    homeboy_local_workspace_dependencies_declared || return 0

    local helper="${HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS:-${collection_dir}/scripts/lib/local-workspace-deps.sh}"
    if [ ! -f "$helper" ]; then
        echo "local_workspace_dependencies declared but helper not found: $helper" >&2
        return 1
    fi
    HOMEBOY_LOCAL_WORKSPACE_DEP_NPM_FLAGS="${HOMEBOY_LOCAL_WORKSPACE_DEP_NPM_FLAGS:---no-audit --no-fund}" \
        bash -c 'source "$1"; homeboy_apply_local_workspace_dependencies "$2"' \
        _ "$helper" "$consumer_dir"
}

# Install a component's npm dependencies so declared local workspace overrides
# are in place BEFORE any consumer lifecycle script runs (homeboy#18302).
#
# Usage: homeboy_npm_install_with_local_workspace <install_dir> <consumer_dir> <collection_dir> <npm argv...>
#
# Without a declaration this runs the install argv unchanged. With one, `npm
# ci`/`npm install` would run the consumer's own `prepare` (often a build)
# against unbuilt `file:` links, so the install runs with --ignore-scripts, the
# overrides build/pack/install each declared dependency, and only then does
# `npm rebuild` run dependency and root install scripts followed by the root
# `prepare` lifecycle (pre/post included).
homeboy_npm_install_with_local_workspace() {
    local install_dir="$1" consumer_dir="$2" collection_dir="$3"
    shift 3
    if ! homeboy_local_workspace_dependencies_declared; then
        (cd "$install_dir" && "$@")
        return
    fi
    echo "[local-workspace-deps] local_workspace_dependencies declared; deferring lifecycle scripts until overrides are installed" >&2
    (cd "$install_dir" && "$@" --ignore-scripts) || return 1
    HOMEBOY_LOCAL_WORKSPACE_DEP_NPM_FLAGS="${HOMEBOY_LOCAL_WORKSPACE_DEP_NPM_FLAGS:---no-audit --no-fund} --ignore-scripts" \
        homeboy_apply_local_workspace_install "$consumer_dir" "$collection_dir" || return 1
    (cd "$install_dir" && npm rebuild) || return 1
    (cd "$install_dir" && npm run prepare --if-present)
}
