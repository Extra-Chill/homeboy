#!/usr/bin/env bash
set -euo pipefail

# Dependency resolution for WordPress extension validation.
#
# Resolves dependencies from two sources (merged, deduplicated):
#   1. Plugin header "Requires Plugins:" (auto-discovered, zero-config)
#   2. Settings JSON "validation_dependencies" / "depends_on" (manual overrides)
#
# Resolution chain for each dependency slug:
#   1. Direct path (if the value is an existing directory)
#   2. Consumer composer.lock vendor package, when present and plugin-shaped
#   3. homeboy component show → local_path (if homeboy is available)
#   4. Git clone from GitHub org (shallow, cached across steps)
#      Org inferred from: HOMEBOY_DEPENDENCY_GITHUB_ORG → git remote origin
#   5. Warn and skip
#
# Settings deps take priority — they can be absolute paths or slugs.
# Header deps are resolved through the same chain by slug.

homeboy_get_validation_dependencies_raw() {
    local settings_json="${HOMEBOY_SETTINGS_JSON:-}"

    if [ -z "$settings_json" ] || [ "$settings_json" = "{}" ]; then
        return 0
    fi

    printf '%s' "$settings_json" | jq -r '.validation_dependencies // .depends_on // empty' 2>/dev/null || true
}

homeboy_normalize_validation_dependencies() {
    local raw="${1:-}"

    if [ -z "$raw" ] || [ "$raw" = "null" ]; then
        return 0
    fi

    if printf '%s' "$raw" | jq -e 'type == "array"' >/dev/null 2>&1; then
        printf '%s' "$raw" | jq -r '.[]'
        return 0
    fi

    if printf '%s' "$raw" | jq -e 'type == "string"' >/dev/null 2>&1; then
        raw=$(printf '%s' "$raw" | jq -r '.')
    fi

    raw=${raw//,/\n}

    while IFS= read -r entry; do
        entry="${entry#${entry%%[![:space:]]*}}"
        entry="${entry%${entry##*[![:space:]]}}"
        [ -n "$entry" ] && printf '%s\n' "$entry"
    done <<< "$raw"
}

# Parse "Requires Plugins:" header from a plugin's main PHP file.
# WordPress format: comma-separated slugs, e.g. "plugin-a, plugin-b"
# Returns one slug per line (trimmed, lowercased).
homeboy_get_requires_plugins_from_header() {
    local plugin_path="${1:-}"

    [ -z "$plugin_path" ] || [ ! -d "$plugin_path" ] && return 0

    # Find the main plugin file (*.php with "Plugin Name:" in root)
    local main_file
    main_file=$(find "$plugin_path" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)

    [ -z "$main_file" ] && return 0

    local requires_line
    requires_line=$(grep -m1 "Requires Plugins:" "$main_file" 2>/dev/null | sed 's/.*Requires Plugins:[[:space:]]*//' | sed 's/[[:space:]]*$//' | tr -d '\r')

    [ -z "$requires_line" ] && return 0

    # Split comma-separated slugs, trim whitespace, output one per line
    local IFS=','
    local slug
    for slug in $requires_line; do
        slug="${slug#${slug%%[![:space:]]*}}"
        slug="${slug%${slug##*[![:space:]]}}"
        [ -n "$slug" ] && printf '%s\n' "$slug"
    done
}

# Resolve the wp-content/plugins/<slug> path segment for a plugin checkout.
# Normal checkouts keep using their directory basename. Worktree-style
# directories may carry a branch suffix (for example data-machine@fix/foo), so
# use the root plugin entry file when it matches the pre-suffix basename.
homeboy_get_validation_dependency_slug() {
    local plugin_path="${1:-}"

    [ -z "$plugin_path" ] || [ ! -d "$plugin_path" ] && return 1

    local dir_slug
    dir_slug="$(basename "$plugin_path")"

    local canonical_dir_slug="${dir_slug%%@*}"
    if [ "$canonical_dir_slug" = "$dir_slug" ]; then
        printf '%s\n' "$dir_slug"
        return 0
    fi

    local main_file
    main_file=$(find "$plugin_path" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)
    if [ -n "$main_file" ]; then
        local main_slug
        main_slug="$(basename "$main_file" .php)"
        if [ "$main_slug" = "$canonical_dir_slug" ]; then
            printf '%s\n' "$main_slug"
            return 0
        fi
    fi

    printf '%s\n' "$dir_slug"
}

_homeboy_is_plugin_shaped_path() {
    local plugin_path="${1:-}"

    [ -z "$plugin_path" ] || [ ! -d "$plugin_path" ] && return 1

    local main_file
    main_file=$(find "$plugin_path" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)
    [ -n "$main_file" ]
}

_homeboy_resolve_composer_locked_dependency_path() {
    local dependency="${1:-}"
    local plugin_path="${2:-}"

    [ -z "$dependency" ] || [ -z "$plugin_path" ] && return 1
    [[ "$dependency" == */* ]] && return 1
    [ -f "${plugin_path}/composer.lock" ] || return 1
    command -v jq >/dev/null 2>&1 || return 1

    local package_name
    package_name=$(jq -r --arg slug "$dependency" '
        [.packages[]?, .["packages-dev"][]?]
        | .[]?
        | .name? // empty
        | select(split("/")[-1] == $slug)
    ' "${plugin_path}/composer.lock" 2>/dev/null | head -1)

    [ -n "$package_name" ] || return 1

    local package_path="${plugin_path}/vendor/${package_name}"
    if _homeboy_is_plugin_shaped_path "$package_path"; then
        printf '%s\n' "$package_path"
        return 0
    fi

    return 1
}

# Infer GitHub org from a git repo's remote.
# Parses "origin" remote URL to extract the org/owner.
# Arg: optional directory to read from (defaults to cwd).
# Returns empty string if not a git repo or can't parse.
_homeboy_infer_github_org() {
    local repo_dir="${1:-.}"
    local remote_url
    remote_url=$(git -C "$repo_dir" remote get-url origin 2>/dev/null || true)

    [ -z "$remote_url" ] && return 0

    # Handle HTTPS and SSH: github.com/ORG/repo or github.com:ORG/repo
    if [[ "$remote_url" =~ github\.com[/:]([^/]+)/ ]]; then
        printf '%s\n' "${BASH_REMATCH[1]}"
        return 0
    fi

    return 0
}

# Get the dependency cache directory for git-cloned deps.
# Uses HOMEBOY_CACHE_DIR/deps/ if set, otherwise a temp directory.
# The directory is created on first use and reused across calls.
_homeboy_get_dep_cache_dir() {
    if [ -n "${_HOMEBOY_DEP_CACHE_DIR:-}" ] && [ -d "$_HOMEBOY_DEP_CACHE_DIR" ]; then
        printf '%s\n' "$_HOMEBOY_DEP_CACHE_DIR"
        return 0
    fi

    local base_dir="${HOMEBOY_CACHE_DIR:-${TMPDIR:-/tmp}}"
    _HOMEBOY_DEP_CACHE_DIR="${base_dir}/homeboy-deps"
    mkdir -p "$_HOMEBOY_DEP_CACHE_DIR" 2>/dev/null || true

    if [ -d "$_HOMEBOY_DEP_CACHE_DIR" ]; then
        printf '%s\n' "$_HOMEBOY_DEP_CACHE_DIR"
        return 0
    fi

    return 1
}

# Clone a dependency from GitHub by slug.
# Uses shallow clone (--depth 1) for speed.
# Returns the clone path if successful, empty otherwise.
_homeboy_clone_dependency() {
    local slug="${1:-}"
    local github_org="${2:-}"

    [ -z "$slug" ] || [ -z "$github_org" ] && return 1

    local cache_dir
    cache_dir=$(_homeboy_get_dep_cache_dir || true)
    [ -z "$cache_dir" ] && return 1

    local clone_path="${cache_dir}/${slug}"

    # Already cloned in this session — reuse
    if [ -d "$clone_path" ]; then
        printf '%s\n' "$clone_path"
        return 0
    fi

    # Build repo URL — use token auth if GITHUB_TOKEN is available (CI environments)
    local repo_url
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        repo_url="https://x-access-token:${GITHUB_TOKEN}@github.com/${github_org}/${slug}.git"
    else
        repo_url="https://github.com/${github_org}/${slug}.git"
    fi

    if git clone --depth 1 --quiet "$repo_url" "$clone_path" 2>/dev/null; then
        printf '%s\n' "$clone_path"
        return 0
    fi

    # Clone failed — clean up partial clone
    rm -rf "$clone_path" 2>/dev/null || true
    return 1
}

_homeboy_known_dependency_github_org() {
    local slug="${1:-}"

    case "$slug" in
        data-machine)
            printf '%s\n' 'Extra-Chill'
            ;;
    esac
}

_homeboy_git_dependency_summary() {
    local repo_dir="${1:-}"

    [ -n "$repo_dir" ] && git -C "$repo_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 || return 0

    local head_sha
    head_sha=$(git -C "$repo_dir" rev-parse --short HEAD 2>/dev/null || true)
    [ -n "$head_sha" ] || return 0

    local branch
    branch=$(git -C "$repo_dir" branch --show-current 2>/dev/null || true)
    [ -n "$branch" ] || branch="detached"

    local summary="HEAD ${head_sha} (${branch})"
    local upstream
    upstream=$(git -C "$repo_dir" rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)
    if [ -n "$upstream" ]; then
        local upstream_sha
        upstream_sha=$(git -C "$repo_dir" rev-parse --short "$upstream" 2>/dev/null || true)
        if [ -n "$upstream_sha" ]; then
            summary+="; upstream ${upstream}@${upstream_sha}"
        else
            summary+="; upstream ${upstream}"
        fi

        local divergence
        divergence=$(git -C "$repo_dir" rev-list --left-right --count "HEAD...${upstream}" 2>/dev/null || true)
        if [ -n "$divergence" ]; then
            local ahead behind
            read -r ahead behind <<< "$divergence"
            summary+="; ahead ${ahead:-0}, behind ${behind:-0}"
        fi
    fi

    printf '%s\n' "$summary"
}

_homeboy_warn_if_dependency_stale() {
    local dependency="${1:-}"
    local repo_dir="${2:-}"

    [ "${HOMEBOY_SUPPRESS_DEPENDENCY_RESOLUTION_LOG:-}" = "1" ] && return 0
    [ -n "$dependency" ] && [ -n "$repo_dir" ] && git -C "$repo_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 || return 0

    local upstream
    upstream=$(git -C "$repo_dir" rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)
    [ -n "$upstream" ] || return 0

    local divergence
    divergence=$(git -C "$repo_dir" rev-list --left-right --count "HEAD...${upstream}" 2>/dev/null || true)
    [ -n "$divergence" ] || return 0

    local ahead behind
    read -r ahead behind <<< "$divergence"
    if [ "${behind:-0}" != "0" ]; then
        echo "Warning: Resolved validation dependency '${dependency}' to local checkout '${repo_dir}', but it is behind ${upstream} by ${behind} commit(s). Update the checkout or pass an explicit dependency path when validating against a newer ref." >&2
    fi
}

_homeboy_report_resolved_dependency() {
    local dependency="${1:-}"
    local source="${2:-}"
    local resolved="${3:-}"

    [ "${HOMEBOY_SUPPRESS_DEPENDENCY_RESOLUTION_LOG:-}" = "1" ] && return 0
    [ -n "$dependency" ] && [ -n "$source" ] && [ -n "$resolved" ] || return 0

    local git_summary
    git_summary=$(_homeboy_git_dependency_summary "$resolved" || true)
    if [ -n "$git_summary" ]; then
        echo "Resolved dependency '${dependency}' via ${source}: ${resolved} (${git_summary})" >&2
    else
        echo "Resolved dependency '${dependency}' via ${source}: ${resolved}" >&2
    fi
}

homeboy_resolve_validation_dependency_path() {
    local dependency="${1:-}"

    [ -z "$dependency" ] && return 1

    # 1. Direct path (absolute or relative directory)
    if [ -d "$dependency" ]; then
        _homeboy_report_resolved_dependency "$dependency" "direct path" "$dependency"
        _homeboy_warn_if_dependency_stale "$dependency" "$dependency"
        printf '%s\n' "$dependency"
        return 0
    fi

    # 2. Prefer the consumer's Composer-locked plugin package when available.
    # This keeps validation aligned with the dependency ref the component actually
    # installed, instead of silently mounting a stale local registry checkout.
    if [ -n "${_HOMEBOY_DEP_PLUGIN_PATH:-}" ]; then
        local composer_resolved
        composer_resolved=$(_homeboy_resolve_composer_locked_dependency_path "$dependency" "${_HOMEBOY_DEP_PLUGIN_PATH}" || true)
        if [ -n "$composer_resolved" ] && [ -d "$composer_resolved" ]; then
            printf '%s\n' "$composer_resolved"
            return 0
        fi
    fi

    # 3. Homeboy component registry lookup
    if command -v homeboy >/dev/null 2>&1; then
        local resolved
        resolved=$(homeboy component show "$dependency" 2>/dev/null | jq -r '.data.entity.local_path // empty' 2>/dev/null || true)
        if [ -n "$resolved" ] && [ -d "$resolved" ]; then
            _homeboy_report_resolved_dependency "$dependency" "Homeboy component registry" "$resolved"
            _homeboy_warn_if_dependency_stale "$dependency" "$resolved"
            printf '%s\n' "$resolved"
            return 0
        fi
    fi

    # 4. Git clone from GitHub org (for CI environments)
    #    Only attempt for slug-like values (no slashes, no absolute paths)
    if [[ "$dependency" != */* ]] && command -v git >/dev/null 2>&1; then
        local github_org="${HOMEBOY_DEPENDENCY_GITHUB_ORG:-}"

        if [ -z "$github_org" ]; then
            # Infer from the component being validated (passed via _HOMEBOY_DEP_PLUGIN_PATH)
            github_org=$(_homeboy_infer_github_org "${_HOMEBOY_DEP_PLUGIN_PATH:-.}" || true)
        fi

        local known_github_org
        known_github_org=$(_homeboy_known_dependency_github_org "$dependency" || true)
        if [ -n "$known_github_org" ]; then
            local cloned_path
            cloned_path=$(_homeboy_clone_dependency "$dependency" "$known_github_org" || true)
            if [ -n "$cloned_path" ] && [ -d "$cloned_path" ]; then
                _homeboy_report_resolved_dependency "$dependency" "git clone from ${known_github_org}/${dependency}" "$cloned_path"
                printf '%s\n' "$cloned_path"
                return 0
            fi
        fi

        if [ -n "$github_org" ] && [ "$github_org" != "$known_github_org" ]; then
            local cloned_path
            cloned_path=$(_homeboy_clone_dependency "$dependency" "$github_org" || true)
            if [ -n "$cloned_path" ] && [ -d "$cloned_path" ]; then
                _homeboy_report_resolved_dependency "$dependency" "git clone from ${github_org}/${dependency}" "$cloned_path"
                printf '%s\n' "$cloned_path"
                return 0
            fi
        fi
    fi

    return 1
}

# Recursive resolver used by homeboy_resolve_validation_dependency_paths(). Bash
# uses dynamic scoping for `local` variables, so this helper intentionally reads
# the caller-owned plugin_path / seen_* maps while keeping the walk logic named
# and testable instead of nesting a function inside the resolver.
_homeboy_walk_validation_dependency() {
    local dependency="${1:-}"

    [ -z "$dependency" ] && return 0

    if [ -n "${seen_dependencies[$dependency]+x}" ]; then
        return 0
    fi
    seen_dependencies["$dependency"]=1

    if [[ "$dependency" != */* ]] && [ -n "${seen_slugs[$dependency]+x}" ]; then
        return 0
    fi

    local resolved
    resolved=$(homeboy_resolve_validation_dependency_path "$dependency" || true)

    if [ -z "$resolved" ]; then
        echo "Warning: Could not resolve WordPress validation dependency '$dependency'" >&2
        return 0
    fi

    if [ -n "$plugin_path" ] && [ "$resolved" = "$plugin_path" ]; then
        return 0
    fi

    local resolved_slug
    resolved_slug=$(homeboy_get_validation_dependency_slug "$resolved" || basename "$resolved")

    # Deduplicate equivalent dependency checkouts by the WordPress plugin slug
    # Playground will mount/load, not just by host path. This prevents a
    # worktree path like data-machine@fix/foo and a canonical data-machine
    # checkout from loading as two copies of the same plugin.
    if [ -n "${seen_slugs[$resolved_slug]+x}" ]; then
        return 0
    fi

    if [ -n "${seen_paths[$resolved]+x}" ]; then
        return 0
    fi

    local transitive_dependency
    while IFS= read -r transitive_dependency; do
        [ -z "$transitive_dependency" ] && continue
        _homeboy_walk_validation_dependency "$transitive_dependency"
    done < <(homeboy_get_requires_plugins_from_header "$resolved" || true)

    if [ -n "${seen_slugs[$resolved_slug]+x}" ] || [ -n "${seen_paths[$resolved]+x}" ]; then
        return 0
    fi

    seen_slugs["$resolved_slug"]=1
    seen_paths["$resolved"]=1

    printf '%s\n' "$resolved"
}

# Emit one dependency path for homeboy_merge_validation_dependency_paths(). Like
# _homeboy_walk_validation_dependency(), this uses the caller's dynamic scope for
# seen_* maps so merge order stays local to each merge call.
_homeboy_emit_merged_validation_dependency_path() {
    local candidate="${1:-}"
    [ -n "$candidate" ] || return 0
    [ -d "$candidate" ] || return 0

    local candidate_slug
    candidate_slug=$(homeboy_get_validation_dependency_slug "$candidate" || basename "$candidate")
    if [ -n "${seen_slugs[$candidate_slug]+x}" ] || [ -n "${seen_paths[$candidate]+x}" ]; then
        return 0
    fi

    seen_slugs["$candidate_slug"]=1
    seen_paths["$candidate"]=1
    printf '%s\n' "$candidate"
}

homeboy_resolve_validation_dependency_paths() {
    local plugin_path="${1:-}"

    # Make plugin path available to the resolver for org inference
    _HOMEBOY_DEP_PLUGIN_PATH="$plugin_path"

    # Collect all dependency identifiers from both sources
    local all_deps=""

    # Source 1: Settings JSON (manual overrides). Explicit configured paths
    # take priority over auto-discovered header slugs so alternate checkouts can
    # replace the canonical local component path.
    local settings_raw
    settings_raw=$(homeboy_get_validation_dependencies_raw)
    if [ -n "$settings_raw" ]; then
        local settings_deps
        settings_deps=$(homeboy_normalize_validation_dependencies "$settings_raw")
        if [ -n "$settings_deps" ]; then
            all_deps="$settings_deps"
        fi
    fi

    # Source 2: Requires Plugins header (auto-discovered)
    local header_deps
    header_deps=$(homeboy_get_requires_plugins_from_header "$plugin_path" || true)
    if [ -n "$header_deps" ]; then
        if [ -n "$all_deps" ]; then
            all_deps="${all_deps}"$'\n'"${header_deps}"
        else
            all_deps="$header_deps"
        fi
    fi

    [ -z "$all_deps" ] && return 0

    # Deduplicate resolved paths and dependency tokens while walking transitive
    # Requires Plugins headers. WordPress enforces those transitive requirements
    # at runtime, so validation needs the same dependency graph when scanning or
    # loading a dependency's implementation classes. Emit transitive dependencies
    # before dependents so Playground loads plugins in WordPress dependency order.
    local -A seen_paths=()
    local -A seen_dependencies=()
    local -A seen_slugs=()
    local dependency

    while IFS= read -r dependency; do
        _homeboy_walk_validation_dependency "$dependency"
    done <<< "$all_deps"
}

homeboy_merge_validation_dependency_paths() {
    local existing_paths="${1:-}"
    local resolved_paths="${2:-}"

    local -A seen_paths=()
    local -A seen_slugs=()
    local -A existing_by_slug=()
    local -a existing_order=()
    local candidate
    local candidate_slug

    while IFS= read -r candidate; do
        [ -n "$candidate" ] || continue
        [ -d "$candidate" ] || continue

        candidate_slug=$(homeboy_get_validation_dependency_slug "$candidate" || basename "$candidate")
        if [ -n "${existing_by_slug[$candidate_slug]+x}" ]; then
            continue
        fi

        existing_by_slug["$candidate_slug"]="$candidate"
        existing_order+=("$candidate_slug")
    done <<< "$existing_paths"

    while IFS= read -r candidate; do
        [ -n "$candidate" ] || continue
        [ -d "$candidate" ] || continue

        candidate_slug=$(homeboy_get_validation_dependency_slug "$candidate" || basename "$candidate")
        if [ -n "${existing_by_slug[$candidate_slug]+x}" ]; then
            _homeboy_emit_merged_validation_dependency_path "${existing_by_slug[$candidate_slug]}"
        else
            _homeboy_emit_merged_validation_dependency_path "$candidate"
        fi
    done <<< "$resolved_paths"

    for candidate_slug in "${existing_order[@]}"; do
        _homeboy_emit_merged_validation_dependency_path "${existing_by_slug[$candidate_slug]}"
    done
}

homeboy_export_validation_dependency_paths() {
    local plugin_path="${1:-}"
    local existing_paths="${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}"
    local resolved_paths
    resolved_paths=$(HOMEBOY_SUPPRESS_DEPENDENCY_RESOLUTION_LOG=1 homeboy_resolve_validation_dependency_paths "$plugin_path" || true)

    local merged_paths
    merged_paths="$(homeboy_merge_validation_dependency_paths "$existing_paths" "$resolved_paths")"
    export HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="$merged_paths"

    local dependency_path
    local dependency_slug
    while IFS= read -r dependency_path; do
        [ -n "$dependency_path" ] || continue
        [ -d "$dependency_path" ] || continue
        dependency_slug=$(homeboy_get_validation_dependency_slug "$dependency_path" || basename "$dependency_path")
        _homeboy_report_resolved_dependency "$dependency_slug" "final validation dependency path" "$dependency_path"
        _homeboy_warn_if_dependency_stale "$dependency_slug" "$dependency_path"
    done <<< "$merged_paths"
}
