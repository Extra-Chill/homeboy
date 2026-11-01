#!/usr/bin/env bash

# Shared post-install bridge for extension dependency runners. Keep build/pack/
# install logic in the Node.js helper; an empty declaration needs no sibling.
homeboy_apply_local_workspace_install() {
    local consumer_dir="$1"
    local collection_dir="$2"
    case "${HOMEBOY_SETTINGS_JSON:-}" in
        ""|"{}") return 0 ;;
    esac
    if node -e '
        try {
            const entries = JSON.parse(process.argv[1]).local_workspace_dependencies;
            process.exit(entries === undefined || (Array.isArray(entries) && entries.length === 0) ? 0 : 1);
        } catch { process.exit(1); }
    ' "$HOMEBOY_SETTINGS_JSON"; then
        return 0
    fi

    local helper="${HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS:-${collection_dir}/nodejs/scripts/lib/local-workspace-deps.sh}"
    if [ ! -f "$helper" ]; then
        echo "local_workspace_dependencies declared but helper not found: $helper" >&2
        return 1
    fi
    HOMEBOY_LOCAL_WORKSPACE_DEP_NPM_FLAGS="${HOMEBOY_LOCAL_WORKSPACE_DEP_NPM_FLAGS:---no-audit --no-fund}" \
        bash -c 'source "$1"; homeboy_apply_local_workspace_dependencies "$2"' \
        _ "$helper" "$consumer_dir"
}
