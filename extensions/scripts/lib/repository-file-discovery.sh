#!/usr/bin/env bash

# Print default lint candidates relative to a repository root. Explicit lint
# targets intentionally bypass this helper so operators can inspect any file.
homeboy_discover_repository_files() {
    local root="$1"
    shift
    local path
    local pattern="${1:-*}"

    if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        while IFS= read -r -d '' path; do
            case "${path#./}" in
                artifacts/*|*/artifacts/*)
                    ;;
                *)
                    printf '%s\0' "$path"
                    ;;
            esac
        done < <(git -C "$root" ls-files --cached --others --exclude-standard -z -- "$@")
        return
    fi

    find "$root" -type f -name "$pattern" -print0
}
