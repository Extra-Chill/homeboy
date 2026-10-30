#!/usr/bin/env bash
# Emit absolute excluded paths. Explicit scope is repo-relative; discovery is
# component-local and only applies when core did not supply the env contract.
homeboy_scope_excludes() {
    local component="$1" root path marker
    component="$(cd "$component" && pwd)"
    if [ "${HOMEBOY_SCOPE_EXCLUDE+x}" = x ]; then
        root="$(git -C "$component" rev-parse --show-toplevel 2>/dev/null)" || root="$component"
        while IFS= read -r path || [ -n "$path" ]; do
            path="${path%$'\r'}"
            [ -n "$path" ] || continue
            path="${path#./}"
            path="${path%/}"
            [ -n "$path" ] || continue
            printf '%s/%s\n' "$root" "$path"
        done <<< "${HOMEBOY_SCOPE_EXCLUDE//,/$'\n'}"
    else
        while IFS= read -r -d '' marker; do
            [ "$marker" = "$component/homeboy.json" ] && continue
            printf '%s\n' "${marker%/homeboy.json}"
        done < <(find "$component" \( -name .git -o -name node_modules -o -name vendor \) -prune -o -type f -name homeboy.json -print0)
    fi
}
