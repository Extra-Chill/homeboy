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
    if ! type homeboy_setting_json >/dev/null 2>&1; then
        local settings_helper="${HOMEBOY_RUNTIME_SETTINGS_HELPER:-}"
        if [ -z "$settings_helper" ]; then
            local shared_lib_dir="${HOMEBOY_SHARED_LIB_DIR:-}"
            if [ -z "$shared_lib_dir" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
                shared_lib_dir="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
            fi
            shared_lib_dir="${shared_lib_dir:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib" && pwd)}"
            # shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
            source "${shared_lib_dir}/runtime-helper-resolver.sh"
            settings_helper="$(homeboy_runtime_helper "${shared_lib_dir%/scripts/lib}" HOMEBOY_RUNTIME_SETTINGS_HELPER settings.sh)" || return 1
        fi
        # shellcheck source=/dev/null
        source "$settings_helper"
    fi

    homeboy_setting_json validation_dependencies 'null' '.validation_dependencies // .depends_on // null'
}

homeboy_normalize_validation_dependencies() {
    local raw="${1:-}"

    if [ -z "$raw" ] || [ "$raw" = "null" ]; then
        return 0
    fi

    if printf '%s' "$raw" | jq -e 'type == "array"' >/dev/null 2>&1; then
        printf '%s' "$raw" | jq -r '.[] | if type == "object" then tojson else tostring end'
        return 0
    fi

    if printf '%s' "$raw" | jq -e 'type == "string"' >/dev/null 2>&1; then
        raw=$(printf '%s' "$raw" | jq -r '.')
    fi

    # Bash parameter substitution does not interpret escape sequences, so
    # `${raw//,/\n}` would splice a literal `n` between entries and weld
    # neighbouring tokens together. Translate commas to real newlines instead.
    raw=$(printf '%s' "$raw" | tr ',' '\n')

    while IFS= read -r entry; do
        entry="${entry#${entry%%[![:space:]]*}}"
        entry="${entry%${entry##*[![:space:]]}}"
        [ -n "$entry" ] && printf '%s\n' "$entry"
    done <<< "$raw"
}

_homeboy_validation_dependency_entry_is_object() {
    local entry="${1:-}"
    [ -n "$entry" ] || return 1
    printf '%s' "$entry" | jq -e 'type == "object"' >/dev/null 2>&1
}

_homeboy_validation_dependency_entry_token() {
    local entry="${1:-}"

    if _homeboy_validation_dependency_entry_is_object "$entry"; then
        printf '%s' "$entry" | jq -r '.dependency // .path // .local_path // .slug // .id // .source // .repo // .repository // .url // empty'
    else
        printf '%s\n' "$entry"
    fi
}

_homeboy_validation_dependency_entry_slug() {
    local entry="${1:-}"

    if _homeboy_validation_dependency_entry_is_object "$entry"; then
        printf '%s' "$entry" | jq -r '.plugin_slug // .slug // .id // empty'
    fi
}

_homeboy_validation_dependency_entry_source_type() {
    local entry="${1:-}"

    if _homeboy_validation_dependency_entry_is_object "$entry"; then
        printf '%s' "$entry" | jq -r '.source_type // .type // empty'
    fi
}

_homeboy_validation_dependency_entry_package_path() {
    local entry="${1:-}"

    if _homeboy_validation_dependency_entry_is_object "$entry"; then
        printf '%s' "$entry" | jq -r '.package_path // .packagePath // .subdir // .subdirectory // .plugin_path // .pluginPath // empty'
    fi
}

_homeboy_validation_dependency_package_root() {
    local resolved_path="${1:-}"
    local package_path="${2:-}"

    [ -n "$resolved_path" ] && [ -d "$resolved_path" ] || return 1
    [ -n "$package_path" ] || {
        printf '%s\n' "$resolved_path"
        return 0
    }

    case "$package_path" in
        /*|*..*) return 1 ;;
    esac

    if [ -d "${resolved_path%/}/${package_path}" ]; then
        printf '%s\n' "${resolved_path%/}/${package_path}"
        return 0
    fi

    return 1
}

_homeboy_validation_dependency_catalog_dir() {
    local base_dir="${HOMEBOY_CACHE_DIR:-${TMPDIR:-/tmp}}"
    local catalog_dir="${base_dir%/}/homeboy-deps"
    mkdir -p "$catalog_dir"
    printf '%s\n' "$catalog_dir"
}

_homeboy_wordpress_org_plugin_zip_url() {
    local slug="${1:-}"
    local version="${2:-}"

    [ -n "$slug" ] || return 1
    if [ -n "$version" ]; then
        printf 'https://downloads.wordpress.org/plugin/%s.%s.zip\n' "$slug" "$version"
    else
        printf 'https://downloads.wordpress.org/plugin/%s.latest-stable.zip\n' "$slug"
    fi
}

_homeboy_clone_catalog_github_dependency() {
    local repo="${1:-}"
    local slug="${2:-}"
    local revision="${3:-}"

    [ -n "$repo" ] || return 1
    [ -n "$slug" ] || slug="$(basename "$repo" .git)"

    local cache_dir repo_url ref_suffix clone_path
    cache_dir=$(_homeboy_validation_dependency_catalog_dir)
    ref_suffix="${revision:-default}"
    ref_suffix=$(printf '%s' "$ref_suffix" | tr -c 'A-Za-z0-9._-' '-')
    clone_path="${cache_dir%/}/${slug}-${ref_suffix}"

    if [ -d "$clone_path" ]; then
        printf '%s\n' "$clone_path"
        return 0
    fi

    if [[ "$repo" == https://* ]] || [[ "$repo" == git@* ]] || [[ "$repo" == ssh://* ]]; then
        repo_url="$repo"
    else
        repo_url="https://github.com/${repo%.git}.git"
    fi

    if [ -n "$revision" ]; then
        git clone --depth 1 --branch "$revision" --quiet "$repo_url" "$clone_path" 2>/dev/null || git clone --quiet "$repo_url" "$clone_path" 2>/dev/null || return 1
        git -C "$clone_path" checkout --quiet "$revision" 2>/dev/null || true
    else
        git clone --depth 1 --quiet "$repo_url" "$clone_path" 2>/dev/null || return 1
    fi

    printf '%s\n' "$clone_path"
}

_homeboy_materialize_wordpress_org_zip_dependency() {
    local slug="${1:-}"
    local version="${2:-}"
    local url="${3:-}"

    [ -n "$slug" ] || return 1
    command -v curl >/dev/null 2>&1 || return 1
    command -v unzip >/dev/null 2>&1 || return 1

    local cache_dir version_suffix target_dir zip_file
    cache_dir=$(_homeboy_validation_dependency_catalog_dir)
    version_suffix="${version:-latest-stable}"
    version_suffix=$(printf '%s' "$version_suffix" | tr -c 'A-Za-z0-9._-' '-')
    target_dir="${cache_dir%/}/${slug}-${version_suffix}"
    zip_file="${target_dir}.zip"

    if [ -d "$target_dir/$slug" ]; then
        printf '%s\n' "$target_dir/$slug"
        return 0
    fi

    [ -n "$url" ] || url=$(_homeboy_wordpress_org_plugin_zip_url "$slug" "$version")
    rm -rf "$target_dir"
    mkdir -p "$target_dir"
    curl -fsSL "$url" -o "$zip_file" || return 1
    unzip -q "$zip_file" -d "$target_dir" || return 1
    rm -f "$zip_file"

    if [ -d "$target_dir/$slug" ]; then
        printf '%s\n' "$target_dir/$slug"
    else
        find "$target_dir" -mindepth 1 -maxdepth 1 -type d -print -quit
    fi
}

_homeboy_resolve_validation_dependency_entry_path() {
    local entry="${1:-}"

    if ! _homeboy_validation_dependency_entry_is_object "$entry"; then
        homeboy_resolve_validation_dependency_path "$entry"
        return $?
    fi

    local source_type source slug revision url package_path resolved package_root
    source_type=$(_homeboy_validation_dependency_entry_source_type "$entry")
    source=$(printf '%s' "$entry" | jq -r '.path // .local_path // .dependency // .source // empty')
    slug=$(_homeboy_validation_dependency_entry_slug "$entry")
    revision=$(printf '%s' "$entry" | jq -r '.revision // .ref // .version // empty')
    url=$(printf '%s' "$entry" | jq -r '.url // .zip_url // empty')
    package_path=$(_homeboy_validation_dependency_entry_package_path "$entry")

    if [ -n "$source" ] && [ -d "$source" ]; then
        package_root=$(_homeboy_validation_dependency_package_root "$source" "$package_path" || true)
        [ -n "$package_root" ] && printf '%s\n' "$package_root" && return 0
    fi

    case "$source_type" in
        github|git|github-repo)
            local repo
            repo=$(printf '%s' "$entry" | jq -r '.repo // .repository // .source // empty')
            resolved=$(_homeboy_clone_catalog_github_dependency "$repo" "$slug" "$revision" || true)
            package_root=$(_homeboy_validation_dependency_package_root "$resolved" "$package_path" || true)
            [ -n "$package_root" ] && [ -d "$package_root" ] && printf '%s\n' "$package_root" && return 0
            ;;
        wp.org|wporg|wordpress.org|wp.org-zip|wordpress.org-zip)
            [ -n "$slug" ] || slug=$(printf '%s' "$entry" | jq -r '.dependency // empty')
            resolved=$(_homeboy_materialize_wordpress_org_zip_dependency "$slug" "$revision" "$url" || true)
            package_root=$(_homeboy_validation_dependency_package_root "$resolved" "$package_path" || true)
            [ -n "$package_root" ] && [ -d "$package_root" ] && printf '%s\n' "$package_root" && return 0
            ;;
    esac

    if [ -n "$source" ]; then
        resolved=$(homeboy_resolve_validation_dependency_path "$source" || true)
        package_root=$(_homeboy_validation_dependency_package_root "$resolved" "$package_path" || true)
        [ -n "$package_root" ] && printf '%s\n' "$package_root" && return 0
        return 1
    fi

    [ -n "$slug" ] || return 1
    homeboy_resolve_validation_dependency_path "$slug"
}

_homeboy_append_resolved_dependency_catalog_entry() {
    local entry="${1:-}"
    local resolved_path="${2:-}"

    [ -n "$resolved_path" ] || return 0
    command -v jq >/dev/null 2>&1 || return 0

    if [ -d "$resolved_path" ]; then
        resolved_path=$(cd "$resolved_path" && pwd -P)
    fi

    local catalog="${_HOMEBOY_RESOLVED_DEPENDENCY_CATALOG_JSON:-[]}" entry_json source_type slug plugin_file version revision activation_status build_commands_json
    if _homeboy_validation_dependency_entry_is_object "$entry"; then
        entry_json="$entry"
    else
        entry_json=$(jq -n --arg dependency "$entry" '{dependency: $dependency}')
    fi
    source_type=$(printf '%s' "$entry_json" | jq -r '.source_type // .type // (if (.source? // .path? // .local_path? // "") | startswith("/") then "local" else "slug" end)')
    slug=$(printf '%s' "$entry_json" | jq -r '.plugin_slug // .slug // .id // empty')
    plugin_file=$(printf '%s' "$entry_json" | jq -r '.plugin_file // .pluginFile // empty')
    version=$(printf '%s' "$entry_json" | jq -r '.version // empty')
    revision=$(printf '%s' "$entry_json" | jq -r '.revision // .ref // empty')
    activation_status=$(printf '%s' "$entry_json" | jq -r 'if has("activate") then (if .activate then "active" else "inactive" end) else (.activation_status // "inactive") end')
    build_commands_json=$(printf '%s' "$entry_json" | jq -c '.build_commands // .build // .prepare_commands // .prepare // [] | if type == "array" then . else [.] end')

    _HOMEBOY_RESOLVED_DEPENDENCY_CATALOG_JSON=$(jq -nc \
        --argjson catalog "$catalog" \
        --argjson entry "$entry_json" \
        --arg sourceType "$source_type" \
        --arg slug "$slug" \
        --arg pluginFile "$plugin_file" \
        --arg version "$version" \
        --arg revision "$revision" \
        --arg activationStatus "$activation_status" \
        --arg resolvedPath "$resolved_path" \
        --argjson buildCommands "$build_commands_json" \
        '$catalog + [($entry + {
            source_type: (if $sourceType == "" then null else $sourceType end),
            slug: (if $slug == "" then null else $slug end),
            plugin_file: (if $pluginFile == "" then null else $pluginFile end),
            requested_version: (if $version == "" then null else $version end),
            requested_revision: (if $revision == "" then null else $revision end),
            build_commands: $buildCommands,
            resolved_path: $resolvedPath,
            activation_status: $activationStatus
        })]')
    export _HOMEBOY_RESOLVED_DEPENDENCY_CATALOG_JSON
    export HOMEBOY_WORDPRESS_DEPENDENCY_CATALOG_JSON="$_HOMEBOY_RESOLVED_DEPENDENCY_CATALOG_JSON"
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
# directories may carry a branch suffix (for example example-plugin@fix/foo), so
# use the root plugin entry file when it matches the pre-suffix basename.
homeboy_get_validation_dependency_slug() {
    local plugin_path="${1:-}"

    [ -z "$plugin_path" ] || [ ! -d "$plugin_path" ] && return 1

    local dir_slug
    dir_slug="$(basename "$plugin_path")"

    local canonical_dir_slug="${dir_slug%%@*}"
    if [ "$canonical_dir_slug" = "$dir_slug" ] && [ "$dir_slug" != "root" ]; then
        printf '%s\n' "$dir_slug"
        return 0
    fi

    if [ -f "${plugin_path}/plugins/${canonical_dir_slug}/${canonical_dir_slug}.php" ] || [ -f "${plugin_path}/plugins/${canonical_dir_slug}/plugin.php" ]; then
        printf '%s\n' "$canonical_dir_slug"
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
        if [ "$dir_slug" = "root" ]; then
            printf '%s\n' "$main_slug"
            return 0
        fi
    fi

    printf '%s\n' "$dir_slug"
}

homeboy_find_validation_dependency_plugin_main_file() {
    local plugin_path="${1:-}"

    [ -n "$plugin_path" ] && [ -d "$plugin_path" ] || return 1

    local candidate canonical_slug
    canonical_slug="$(basename "$plugin_path")"
    canonical_slug="${canonical_slug%%@*}"
    for candidate in "${plugin_path}/$(basename "$plugin_path").php" "${plugin_path}/plugin.php"; do
        [ -f "$candidate" ] || continue
        if grep -q 'Plugin Name:' "$candidate"; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    for candidate in "$plugin_path"/*.php; do
        [ -f "$candidate" ] || continue
        if grep -q 'Plugin Name:' "$candidate"; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    for candidate in \
        "${plugin_path}/packages/wordpress-plugin/${canonical_slug}.php" \
        "${plugin_path}/packages/wordpress-plugin/plugin.php"; do
        [ -f "$candidate" ] || continue
        if grep -q 'Plugin Name:' "$candidate"; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    for candidate in "${plugin_path}/packages/wordpress-plugin"/*.php; do
        [ -f "$candidate" ] || continue
        if grep -q 'Plugin Name:' "$candidate"; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

homeboy_find_validation_dependency_plugin_package_root() {
    local plugin_path="${1:-}"
    local dependency_slug="${2:-}"
    local plugin_file

    [ -n "$plugin_path" ] && [ -d "$plugin_path" ] || return 1

    plugin_file=$(homeboy_find_validation_dependency_plugin_main_file "$plugin_path" || true)
    if [ -n "$plugin_file" ]; then
        printf '%s\n' "$plugin_path"
        return 0
    fi

    [ -n "$dependency_slug" ] || dependency_slug=$(homeboy_get_validation_dependency_slug "$plugin_path" || basename "$plugin_path")
    plugin_file=$(_homeboy_wordpress_dependency_preflight_source_checkout_plugin_file "$plugin_path" "$dependency_slug" || true)
    if [ -n "$plugin_file" ]; then
        dirname "$plugin_file"
        return 0
    fi

    return 1
}

_homeboy_wordpress_dependency_preflight_diagnostics_file() {
    local artifacts_dir="${1:-}"

    [ -n "$artifacts_dir" ] || return 1
    printf '%s\n' "${artifacts_dir%/}/wordpress-dependency-plugin-preflight-diagnostics.json"
}

_homeboy_wordpress_dependency_preflight_append_diagnostic() {
    local artifacts_dir="${1:-}"
    local code="${2:-wordpress-dependency-plugin-preflight-failed}"
    local message="${3:-WordPress dependency plugin preflight failed.}"
    local context="${4:-wordpress}"
    local slug="${5:-}"
    local dependency_path="${6:-}"
    local expected_plugin_file="${7:-}"
    local plugin_file="${8:-}"
    local missing_include="${9:-}"
    local exit_code="${10:-}"
    local output="${11:-}"
    local package_required="${12:-false}"
    local source_checkout_plugin_file="${13:-}"

    [ -n "$artifacts_dir" ] || return 0
    command -v jq >/dev/null 2>&1 || return 0

    local diagnostics_file existing_json tmp_file
    diagnostics_file=$(_homeboy_wordpress_dependency_preflight_diagnostics_file "$artifacts_dir")
    mkdir -p "$(dirname "$diagnostics_file")"
    existing_json='{"schema":"homeboy/wordpress-dependency-plugin-preflight/v1","diagnostics":[]}'
    if [ -f "$diagnostics_file" ]; then
        existing_json=$(jq -c 'if type == "object" and (.diagnostics | type) == "array" then . else {schema:"homeboy/wordpress-dependency-plugin-preflight/v1", diagnostics:[]} end' "$diagnostics_file" 2>/dev/null || printf '%s\n' "$existing_json")
    fi

    tmp_file=$(mktemp "${diagnostics_file}.XXXXXX")
    jq -n \
        --argjson existing "$existing_json" \
        --arg schema 'homeboy/wordpress-dependency-plugin-preflight/v1' \
        --arg code "$code" \
        --arg message "$message" \
        --arg context "$context" \
        --arg slug "$slug" \
        --arg dependencyPath "$dependency_path" \
        --arg expectedPluginFile "$expected_plugin_file" \
        --arg pluginFile "$plugin_file" \
        --arg missingInclude "$missing_include" \
        --arg exitCode "$exit_code" \
        --arg output "$output" \
        --argjson packageRequired "$package_required" \
        --arg sourceCheckoutPluginFile "$source_checkout_plugin_file" \
        '$existing + {schema: $schema} | .diagnostics += [{
            code: $code,
            severity: "error",
            phase: "dependency-preflight",
            context: $context,
            message: $message,
            dependency_slug: (if $slug == "" then null else $slug end),
            dependency_path: (if $dependencyPath == "" then null else $dependencyPath end),
            expected_plugin_file: (if $expectedPluginFile == "" then null else $expectedPluginFile end),
            plugin_file: (if $pluginFile == "" then null else $pluginFile end),
            missing_include: (if $missingInclude == "" then null else $missingInclude end),
            package_required: $packageRequired,
            source_checkout_plugin_file: (if $sourceCheckoutPluginFile == "" then null else $sourceCheckoutPluginFile end),
            exit_code: (if $exitCode == "" then null else ($exitCode | tonumber) end),
            output: (if $output == "" then null else $output end)
        }]' > "$tmp_file"
    mv "$tmp_file" "$diagnostics_file"
}

_homeboy_wordpress_dependency_preflight_source_checkout_plugin_file() {
    local dependency_path="${1:-}"
    local dependency_slug="${2:-}"
    local candidate

    [ -n "$dependency_path" ] && [ -d "$dependency_path" ] || return 1

    for candidate in \
        "${dependency_path}/plugins/${dependency_slug}/${dependency_slug}.php" \
        "${dependency_path}/plugins/${dependency_slug}/plugin.php"; do
        [ -f "$candidate" ] || continue
        if grep -q 'Plugin Name:' "$candidate"; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

_homeboy_wordpress_dependency_preflight_extract_missing_include() {
    local output="${1:-}"
    local missing=""

    missing=$(printf '%s\n' "$output" | sed -n -E "s/.*Failed opening required '([^']+)'.*/\1/p; s/.*Failed opening '([^']+)'.*/\1/p" | head -1)
    if [ -n "$missing" ]; then
        printf '%s\n' "$missing"
        return 0
    fi

    missing=$(printf '%s\n' "$output" | sed -n -E 's/.*(require_once|require|include_once|include)\(([^)]+)\): Failed to open stream.*/\2/p' | head -1)
    [ -n "$missing" ] && printf '%s\n' "$missing"
}

_homeboy_wordpress_dependency_preflight_php_load() {
    local plugin_file="${1:-}"
    local tmp_file output exit_code

    [ -n "$plugin_file" ] && [ -f "$plugin_file" ] || return 1

    tmp_file=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-dependency-preflight.XXXXXX.php")
    cat > "$tmp_file" <<'PHP'
<?php
error_reporting(E_ALL);
ini_set('display_errors', '1');
define('ABSPATH', sys_get_temp_dir() . '/homeboy-wordpress-preflight/');
define('WPINC', 'wp-includes');
foreach (array(
    'add_action', 'add_filter', 'remove_action', 'remove_filter', 'do_action', 'do_action_ref_array',
    'register_activation_hook', 'register_deactivation_hook', 'register_uninstall_hook',
    'wp_register_script', 'wp_enqueue_script', 'wp_register_style', 'wp_enqueue_style',
) as $function) {
    if (!function_exists($function)) {
        eval('function ' . $function . '(...$args) { return true; }');
    }
}
if (!function_exists('apply_filters')) { function apply_filters($hook, $value = null, ...$args) { return $value; } }
if (!function_exists('apply_filters_ref_array')) { function apply_filters_ref_array($hook, $args) { return $args[0] ?? null; } }
if (!function_exists('plugin_dir_path')) { function plugin_dir_path($file) { return rtrim(dirname($file), '/\\') . '/'; } }
if (!function_exists('plugin_dir_url')) { function plugin_dir_url($file) { return ''; } }
if (!function_exists('plugin_basename')) { function plugin_basename($file) { return basename(dirname($file)) . '/' . basename($file); } }
if (!function_exists('trailingslashit')) { function trailingslashit($string) { return rtrim($string, '/\\') . '/'; } }
if (!function_exists('sanitize_key')) { function sanitize_key($key) { return preg_replace('/[^a-z0-9_\\-]/', '', strtolower((string) $key)); } }
if (!function_exists('sanitize_text_field')) { function sanitize_text_field($str) { return trim(strip_tags((string) $str)); } }
if (!function_exists('wp_normalize_path')) { function wp_normalize_path($path) { return str_replace('\\', '/', $path); } }
if (!function_exists('wp_die')) { function wp_die($message = '', $title = '', $args = array()) { fwrite(STDERR, (string) $message); exit(1); } }
if (!function_exists('__')) { function __($text, $domain = 'default') { return $text; } }
if (!function_exists('_e')) { function _e($text, $domain = 'default') { echo $text; } }
if (!function_exists('esc_html__')) { function esc_html__($text, $domain = 'default') { return $text; } }
if (!function_exists('esc_attr__')) { function esc_attr__($text, $domain = 'default') { return $text; } }
if (!function_exists('esc_html')) { function esc_html($text) { return $text; } }
if (!function_exists('esc_attr')) { function esc_attr($text) { return $text; } }
if (!function_exists('esc_url')) { function esc_url($url) { return $url; } }
if (!function_exists('is_admin')) { function is_admin() { return false; } }
if (!function_exists('is_multisite')) { function is_multisite() { return false; } }
if (!function_exists('get_option')) { function get_option($option, $default = false) { return $default; } }
if (!function_exists('get_site_option')) { function get_site_option($option, $default = false) { return $default; } }
if (!function_exists('current_user_can')) { function current_user_can($capability, ...$args) { return false; } }
require_once $argv[1];
PHP

    set +e
    output=$(php "$tmp_file" "$plugin_file" 2>&1)
    exit_code=$?
    set -e
    rm -f "$tmp_file"

    if [ "$exit_code" -ne 0 ]; then
        printf '%s\n' "$exit_code"
        printf '%s\n' "$output"
        return 1
    fi

    return 0
}

homeboy_preflight_declared_validation_dependency_paths() {
    local artifacts_dir="${1:-}"
    local context="${2:-wordpress}"
    local raw configured dependency
    local failed=0

    raw="${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}"
    configured=$(homeboy_normalize_validation_dependencies "$(homeboy_get_validation_dependencies_raw || true)" || true)
    if [ -n "$configured" ]; then
        raw="${raw}"$'\n'"${configured}"
    fi

    while IFS= read -r dependency; do
        [ -n "$dependency" ] || continue
        if _homeboy_validation_dependency_entry_is_object "$dependency"; then
            dependency=$(_homeboy_resolve_validation_dependency_entry_path "$dependency" || true)
            [ -n "$dependency" ] || continue
        fi
        case "$dependency" in
            /*|./*|../*|*/*)
                if [ ! -d "$dependency" ]; then
                    _homeboy_wordpress_dependency_preflight_append_diagnostic \
                        "$artifacts_dir" \
                        'wordpress-dependency-path-missing' \
                        "WordPress dependency plugin path does not exist: ${dependency}" \
                        "$context" \
                        "$(basename "$dependency")" \
                        "$dependency" \
                        "$dependency" \
                        '' \
                        '' \
                        '' \
                        '' \
                        false \
                        ''
                    echo "Error: WordPress dependency plugin path does not exist: ${dependency}" >&2
                    failed=1
                fi
                ;;
        esac
    done <<< "$raw"

    [ "$failed" -eq 0 ]
}

homeboy_preflight_wordpress_dependency_plugins() {
    local dependency_paths="${1:-}"
    local artifacts_dir="${2:-}"
    local context="${3:-wordpress}"
    local dependency_path dependency_slug expected_plugin_file plugin_file nested_plugin_file load_result load_exit load_output missing_include
    local failed=0

    while IFS= read -r dependency_path; do
        [ -n "$dependency_path" ] || continue

        dependency_slug=$(homeboy_get_validation_dependency_slug "$dependency_path" || basename "$dependency_path")
        expected_plugin_file="${dependency_path%/}/${dependency_slug}.php"

        if [ ! -d "$dependency_path" ]; then
            _homeboy_wordpress_dependency_preflight_append_diagnostic \
                "$artifacts_dir" \
                'wordpress-dependency-path-missing' \
                "WordPress dependency plugin path does not exist: ${dependency_path}" \
                "$context" "$dependency_slug" "$dependency_path" "$expected_plugin_file" '' '' '' '' false ''
            echo "Error: WordPress dependency plugin '${dependency_slug}' path does not exist: ${dependency_path}" >&2
            failed=1
            continue
        fi

        if [ "$context" = "bench" ]; then
            local package_root
            package_root=$(homeboy_find_validation_dependency_plugin_package_root "$dependency_path" "$dependency_slug" || true)
            if [ -n "$package_root" ] && [ "$package_root" != "$dependency_path" ]; then
                dependency_path="$package_root"
                dependency_slug=$(homeboy_get_validation_dependency_slug "$dependency_path" || basename "$dependency_path")
                expected_plugin_file="${dependency_path%/}/${dependency_slug}.php"
            fi
        fi

        plugin_file=$(homeboy_find_validation_dependency_plugin_main_file "$dependency_path" || true)
        if [ -z "$plugin_file" ]; then
            nested_plugin_file=$(_homeboy_wordpress_dependency_preflight_source_checkout_plugin_file "$dependency_path" "$dependency_slug" || true)
            _homeboy_wordpress_dependency_preflight_append_diagnostic \
                "$artifacts_dir" \
                'wordpress-dependency-plugin-main-file-missing' \
                "WordPress dependency plugin '${dependency_slug}' is not a runnable plugin package; no plugin main file was found at the dependency root." \
                "$context" "$dependency_slug" "$dependency_path" "$expected_plugin_file" '' '' '' '' true "$nested_plugin_file"
            echo "Error: WordPress dependency plugin '${dependency_slug}' is not a runnable plugin package: ${dependency_path}" >&2
            echo "  Expected plugin main file: ${expected_plugin_file}" >&2
            if [ -n "$nested_plugin_file" ]; then
                echo "  Found source-checkout plugin file: ${nested_plugin_file}" >&2
            fi
            echo "  Use a packaged plugin build for WordPress runtime bench/trace evidence." >&2
            failed=1
            continue
        fi

        if [ "$context" = "bench" ]; then
            continue
        fi

        load_result=$(_homeboy_wordpress_dependency_preflight_php_load "$plugin_file" || true)
        if [ -n "$load_result" ]; then
            load_exit=$(printf '%s\n' "$load_result" | head -1)
            load_output=$(printf '%s\n' "$load_result" | tail -n +2 | head -c 4000)
            missing_include=$(_homeboy_wordpress_dependency_preflight_extract_missing_include "$load_output" || true)
            _homeboy_wordpress_dependency_preflight_append_diagnostic \
                "$artifacts_dir" \
                'wordpress-dependency-plugin-load-fatal' \
                "WordPress dependency plugin '${dependency_slug}' failed a lightweight PHP load preflight before WP Codebox dispatch." \
                "$context" "$dependency_slug" "$dependency_path" "$expected_plugin_file" "$plugin_file" "$missing_include" "$load_exit" "$load_output" true ''
            echo "Error: WordPress dependency plugin '${dependency_slug}' failed PHP load preflight before WP Codebox dispatch." >&2
            echo "  Plugin file: ${plugin_file}" >&2
            [ -z "$missing_include" ] || echo "  Missing include/build artifact: ${missing_include}" >&2
            echo "  Use a packaged plugin build if this checkout needs generated runtime artifacts." >&2
            failed=1
        fi
    done <<< "$dependency_paths"

    [ "$failed" -eq 0 ]
}

_homeboy_is_plugin_shaped_path() {
    local plugin_path="${1:-}"

    [ -z "$plugin_path" ] || [ ! -d "$plugin_path" ] && return 1

    local main_file
    main_file=$(find "$plugin_path" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)
    [ -n "$main_file" ]
}

_homeboy_lab_workspace_mappings_json() {
    local raw="${HOMEBOY_LAB_WORKSPACE_MAPPINGS_JSON:-${HOMEBOY_LAB_WORKSPACE_MAP_JSON:-}}"

    if [ -z "$raw" ] && [ -n "${HOMEBOY_LAB_OFFLOAD_JSON:-}" ]; then
        raw="$HOMEBOY_LAB_OFFLOAD_JSON"
    fi

    [ -n "$raw" ] || return 1

    printf '%s\n' "$raw"
}

_homeboy_emit_lab_workspace_mappings() {
    command -v jq >/dev/null 2>&1 || return 0

    local raw
    raw=$(_homeboy_lab_workspace_mappings_json || true)
    [ -n "$raw" ] || return 0

    printf '%s' "$raw" | jq -r '
        def mapping_stream:
            if type == "array" then
                .[]
                | {
                    local: (.local_path // .local // .source // .from // empty),
                    remote: (.remote_path // .remote // .target // .to // empty)
                }
            elif type == "object" then
                to_entries[]
                | {
                    local: .key,
                    remote: (
                        if (.value | type) == "object" then
                            (.value.remote_path // .value.remote // .value.target // .value.to // empty)
                        else
                            .value
                        end
                    )
                }
            else
                empty
            end;

        if type == "object" then
            (.workspace_mappings // .workspace_mapping // .workspace_map // .workspaces // .dependency_workspaces // .dependencies // .)
        else
            .
        end
        | mapping_stream
        | select(.local != "" and .remote != "")
        | [.local, .remote]
        | @tsv
    ' 2>/dev/null || true
}

homeboy_translate_lab_workspace_path() {
    local path_value="${1:-}"
    [ -n "$path_value" ] || return 1

    local best_local=""
    local best_remote=""
    local local_path remote_path local_len
    while IFS=$'\t' read -r local_path remote_path; do
        [ -n "$local_path" ] && [ -n "$remote_path" ] || continue
        case "$path_value" in
            "$local_path"|"$local_path"/*)
                local_len=${#local_path}
                if [ "$local_len" -gt "${#best_local}" ]; then
                    best_local="$local_path"
                    best_remote="$remote_path"
                fi
                ;;
        esac
    done < <(_homeboy_emit_lab_workspace_mappings)

    if [ -n "$best_local" ]; then
        printf '%s%s\n' "${best_remote%/}" "${path_value#"$best_local"}"
        return 0
    fi

    printf '%s\n' "$path_value"
}

homeboy_translate_validation_dependency_paths() {
    local dependency_paths="${1:-}"
    local dependency_path translated_path

    while IFS= read -r dependency_path; do
        [ -n "$dependency_path" ] || continue
        translated_path=$(homeboy_translate_lab_workspace_path "$dependency_path" || true)
        [ -n "$translated_path" ] && printf '%s\n' "$translated_path"
    done <<< "$dependency_paths"
}

_homeboy_resolve_lab_mapped_dependency_path() {
    local dependency="${1:-}"
    [ -n "$dependency" ] || return 1

    local translated
    translated=$(homeboy_translate_lab_workspace_path "$dependency" || true)
    if [ -n "$translated" ] && [ "$translated" != "$dependency" ] && [ -d "$translated" ]; then
        _homeboy_report_resolved_dependency "$dependency" "Lab workspace mapping" "$translated"
        printf '%s\n' "$translated"
        return 0
    fi

    [[ "$dependency" == */* ]] && return 1

    local local_path remote_path remote_slug local_slug remote_basename local_basename
    while IFS=$'\t' read -r local_path remote_path; do
        [ -n "$remote_path" ] && [ -d "$remote_path" ] || continue

        remote_slug=$(homeboy_get_validation_dependency_slug "$remote_path" || true)
        remote_basename="$(basename "$remote_path")"
        local_basename="$(basename "$local_path")"
        local_slug="${local_basename%%@*}"

        if [ "$dependency" = "$remote_slug" ] || [ "$dependency" = "$remote_basename" ] || [ "$dependency" = "$local_slug" ] || [ "$dependency" = "$local_basename" ]; then
            _homeboy_report_resolved_dependency "$dependency" "Lab workspace mapping" "$remote_path"
            printf '%s\n' "$remote_path"
            return 0
        fi
    done < <(_homeboy_emit_lab_workspace_mappings)

    return 1
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
        if ! _homeboy_prepare_cloned_dependency "$clone_path"; then
            rm -rf "$clone_path" 2>/dev/null || true
            return 1
        fi
        printf '%s\n' "$clone_path"
        return 0
    fi

    # Clone failed — clean up partial clone
    rm -rf "$clone_path" 2>/dev/null || true
    return 1
}

_homeboy_prepare_cloned_dependency() {
    local clone_path="${1:-}"

    [ -n "$clone_path" ] && [ -d "$clone_path" ] || return 1
    [ -f "${clone_path}/composer.json" ] || return 0
    [ ! -f "${clone_path}/vendor/autoload.php" ] || return 0

    if ! command -v composer >/dev/null 2>&1; then
        echo "Warning: Validation dependency '${clone_path}' has composer.json but composer is not available." >&2
        return 0
    fi

    # `--no-plugins`: dependencies are installed only to supply an autoloader
    # and vendor tree for analysis and test bootstrapping. Composer plugins
    # such as `composer/installers` exist to relocate packages inside a real
    # WordPress tree, which this copy is not, and running them would make
    # preparation depend on the host's ambient `allow-plugins` policy.
    if ! ( cd "$clone_path" && composer install --no-dev --no-interaction --no-plugins --quiet ); then
        echo "Warning: Could not install Composer dependencies for validation dependency '${clone_path}'." >&2
        return 1
    fi
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
    local direct_path="$dependency"

    [ -z "$dependency" ] && return 1

    # Settings are component configuration. Resolve relative paths from that
    # component rather than the runner's transient working directory.
    if [[ "$direct_path" != /* ]] && [ -n "${_HOMEBOY_DEP_PLUGIN_PATH:-}" ]; then
        direct_path="${_HOMEBOY_DEP_PLUGIN_PATH%/}/${direct_path#./}"
    fi

    local lab_mapped
    lab_mapped=$(_homeboy_resolve_lab_mapped_dependency_path "$dependency" || true)
    if [ -n "$lab_mapped" ] && [ -d "$lab_mapped" ]; then
        printf '%s\n' "$lab_mapped"
        return 0
    fi

    # 1. Direct path. Relative settings paths are anchored to the component
    # workspace, then canonicalized before callers embed them in generated
    # config files that may live outside that workspace.
    #
    # A bare slug is only satisfied by a directory that is actually a plugin.
    # Components legitimately contain subdirectories named after a dependency
    # for other reasons — a bbPress theme-compat template override directory is
    # the canonical example — and treating those as the dependency mounts a
    # non-plugin as a plugin and hides the real one. Explicit paths keep their
    # existing behaviour below, so a deliberate `./path` or `/abs/path` still
    # reports the location the operator named.
    if [ -d "$direct_path" ]; then
        local direct_path_is_slug=1
        case "$dependency" in
            /*|./*|../*) direct_path_is_slug=0 ;;
        esac

        if [ "$direct_path_is_slug" -eq 0 ] || _homeboy_is_plugin_shaped_path "$direct_path"; then
            direct_path=$(cd "$direct_path" && pwd -P)
            _homeboy_report_resolved_dependency "$dependency" "direct path" "$direct_path"
            _homeboy_warn_if_dependency_stale "$dependency" "$direct_path"
            printf '%s\n' "$direct_path"
            return 0
        fi

        echo "Note: Ignoring '${direct_path}' for validation dependency '${dependency}': directory is not plugin-shaped (no root PHP file with a 'Plugin Name:' header). Continuing dependency resolution." >&2
    fi

    # An explicitly relative or absolute path is not a dependency slug. Return
    # its component-rooted location so the consuming validation tool can report
    # the missing path instead of silently treating a configuration typo as an
    # unresolved plugin name.
    case "$dependency" in
        /*|./*|../*)
            printf '%s\n' "$direct_path"
            return 0
            ;;
    esac

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

        if [ -n "$github_org" ]; then
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

    local dependency_token
    dependency_token=$(_homeboy_validation_dependency_entry_token "$dependency" || true)
    [ -n "$dependency_token" ] || return 0

    if [ -n "${seen_dependencies[$dependency_token]+x}" ]; then
        return 0
    fi
    seen_dependencies["$dependency_token"]=1

    if [[ "$dependency_token" != */* ]] && [ -n "${seen_slugs[$dependency_token]+x}" ]; then
        return 0
    fi

    local resolved
    resolved=$(_homeboy_resolve_validation_dependency_entry_path "$dependency" || true)

    if [ -z "$resolved" ]; then
        echo "Warning: Could not resolve WordPress validation dependency '$dependency_token'" >&2
        return 0
    fi

    if [ -n "$plugin_path" ] && [ "$resolved" = "$plugin_path" ]; then
        return 0
    fi

    local resolved_slug
    resolved_slug=$(homeboy_get_validation_dependency_slug "$resolved" || basename "$resolved")

    # Deduplicate equivalent dependency checkouts by the WordPress plugin slug
    # Playground will mount/load, not just by host path. This prevents a
    # worktree path like example-plugin@fix/foo and a canonical example-plugin
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
    _homeboy_append_resolved_dependency_catalog_entry "$dependency" "$resolved"

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

homeboy_dependency_needs_composer_prepare() {
    local dependency_path="${1:-}"

    [ -n "$dependency_path" ] && [ -d "$dependency_path" ] || return 1
    [ -f "${dependency_path}/composer.json" ] || return 1
    [ ! -f "${dependency_path}/vendor/autoload.php" ] || return 1
    [ ! -f "${dependency_path}/vendor/autoload_packages.php" ] || return 1

    return 0
}

_homeboy_dependency_repo_root() {
    local dependency_path="${1:-}"

    [ -n "$dependency_path" ] && [ -d "$dependency_path" ] || return 1
    git -C "$dependency_path" rev-parse --show-toplevel 2>/dev/null || printf '%s\n' "$dependency_path"
}

_homeboy_copy_dependency_prepare_root() {
    local source_root="${1:-}"
    local target_root="${2:-}"

    [ -n "$source_root" ] && [ -d "$source_root" ] || return 1
    [ -n "$target_root" ] || return 1

    rm -rf "$target_root"
    mkdir -p "$target_root"

    if command -v rsync >/dev/null 2>&1; then
        rsync -a --delete --exclude '.git' "${source_root}/" "${target_root}/"
    else
        ( cd "$source_root" && tar --exclude './.git' -cf - . ) | ( cd "$target_root" && tar -xf - )
    fi
}

_homeboy_sha256_string() {
    local value="${1:-}"

    if [ -z "$value" ]; then
        value=$(cat)
    fi

    printf '%s' "$value" | shasum -a 256 | awk '{print $1}'
}

_homeboy_sha256_file() {
    local file_path="${1:-}"

    if [ -n "$file_path" ] && [ -f "$file_path" ]; then
        shasum -a 256 "$file_path" | awk '{print $1}'
    else
        printf '%s\n' 'missing'
    fi
}

_homeboy_prepared_dependency_php_version() {
    command -v php >/dev/null 2>&1 || return 0
    php -r 'echo PHP_VERSION;' 2>/dev/null || true
}

_homeboy_prepared_dependency_git_state() {
    local prepare_root="${1:-}"

    [ -n "$prepare_root" ] && git -C "$prepare_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
        printf '%s\n' 'none'
        return 0
    }

    local head_sha dirty_hash
    head_sha=$(git -C "$prepare_root" rev-parse HEAD 2>/dev/null || true)
    dirty_hash=$({ git -C "$prepare_root" status --porcelain=v1 2>/dev/null; git -C "$prepare_root" diff --binary 2>/dev/null; git -C "$prepare_root" diff --cached --binary 2>/dev/null; } | shasum -a 256 | awk '{print $1}')
    printf '%s:%s\n' "${head_sha:-unknown}" "${dirty_hash:-unknown}"
}

_homeboy_prepared_dependency_catalog_entry() {
    local dependency_path="${1:-}"
    local package_root="${2:-}"
    local catalog_json="${HOMEBOY_WORDPRESS_DEPENDENCY_CATALOG_JSON:-${_HOMEBOY_RESOLVED_DEPENDENCY_CATALOG_JSON:-[]}}"

    [ -n "$dependency_path" ] || return 0
    command -v jq >/dev/null 2>&1 || return 0

    local dependency_realpath package_realpath
    dependency_realpath=$(cd "$dependency_path" && pwd -P 2>/dev/null || printf '%s\n' "$dependency_path")
    if [ -n "$package_root" ] && [ -d "$package_root" ]; then
        package_realpath=$(cd "$package_root" && pwd -P 2>/dev/null || printf '%s\n' "$package_root")
    else
        package_realpath="$dependency_realpath"
    fi

    local catalog_entry
    catalog_entry=$(printf '%s' "$catalog_json" | jq -c --arg dependencyPath "$dependency_realpath" --arg packageRoot "$package_realpath" '
        if type == "array" then . else [] end
        | map(select((.resolved_path // "") == $dependencyPath or (.resolved_path // "") == $packageRoot))
        | .[0] // {}
    ' 2>/dev/null || printf '{}\n')

    if printf '%s' "$catalog_entry" | jq -e 'type == "object" and length > 0' >/dev/null 2>&1; then
        printf '%s\n' "$catalog_entry"
        return 0
    fi

    local settings_raw settings_deps entry entry_path entry_realpath entry_slug dependency_slug
    settings_raw=$(homeboy_get_validation_dependencies_raw || true)
    settings_deps=$(homeboy_normalize_validation_dependencies "$settings_raw" || true)
    dependency_slug=$(homeboy_get_validation_dependency_slug "$package_realpath" || basename "$package_realpath")
    while IFS= read -r entry; do
        [ -n "$entry" ] || continue
        _homeboy_validation_dependency_entry_is_object "$entry" || continue
        entry_path=$(_homeboy_resolve_validation_dependency_entry_path "$entry" 2>/dev/null || true)
        if [ -n "$entry_path" ] && [ -d "$entry_path" ]; then
            entry_realpath=$(cd "$entry_path" && pwd -P)
            if [ "$entry_realpath" = "$dependency_realpath" ] || [ "$entry_realpath" = "$package_realpath" ]; then
                _homeboy_catalog_entry_from_dependency_object "$entry" "$entry_realpath"
                return 0
            fi
        fi

        entry_slug=$(_homeboy_validation_dependency_entry_slug "$entry" || true)
        if [ -n "$entry_slug" ] && [ "$entry_slug" = "$dependency_slug" ]; then
            _homeboy_catalog_entry_from_dependency_object "$entry" "$dependency_realpath"
            return 0
        fi
    done <<< "$settings_deps"

    printf '{}\n'
}

_homeboy_catalog_entry_from_dependency_object() {
    local entry_json="${1:-}"
    local resolved_path="${2:-}"

    [ -n "$entry_json" ] || {
        printf '{}\n'
        return 0
    }

    local source_type slug plugin_file version revision activation_status build_commands_json
    source_type=$(printf '%s' "$entry_json" | jq -r '.source_type // .type // empty')
    slug=$(printf '%s' "$entry_json" | jq -r '.plugin_slug // .slug // .id // empty')
    plugin_file=$(printf '%s' "$entry_json" | jq -r '.plugin_file // .pluginFile // empty')
    version=$(printf '%s' "$entry_json" | jq -r '.version // empty')
    revision=$(printf '%s' "$entry_json" | jq -r '.revision // .ref // empty')
    activation_status=$(printf '%s' "$entry_json" | jq -r 'if has("activate") then (if .activate then "active" else "inactive" end) else (.activation_status // "inactive") end')
    build_commands_json=$(printf '%s' "$entry_json" | jq -c '.build_commands // .build // .prepare_commands // .prepare // [] | if type == "array" then . else [.] end')

    jq -nc \
        --argjson entry "$entry_json" \
        --arg sourceType "$source_type" \
        --arg slug "$slug" \
        --arg pluginFile "$plugin_file" \
        --arg version "$version" \
        --arg revision "$revision" \
        --arg activationStatus "$activation_status" \
        --arg resolvedPath "$resolved_path" \
        --argjson buildCommands "$build_commands_json" \
        '$entry + {
            source_type: (if $sourceType == "" then null else $sourceType end),
            slug: (if $slug == "" then null else $slug end),
            plugin_file: (if $pluginFile == "" then null else $pluginFile end),
            requested_version: (if $version == "" then null else $version end),
            requested_revision: (if $revision == "" then null else $revision end),
            build_commands: $buildCommands,
            resolved_path: $resolvedPath,
            activation_status: $activationStatus
        }'
}

_homeboy_composer_runtime_requirements() {
    local package_path="${1:-}"

    [ -n "$package_path" ] && [ -f "${package_path%/}/composer.json" ] || {
        printf '{}\n'
        return 0
    }

    jq -c '.require // {}' "${package_path%/}/composer.json" 2>/dev/null || printf '{}\n'
}

_homeboy_prepared_dependency_cache_dir() {
    local artifacts_dir="${1:-}"
    local base_dir="${HOMEBOY_WP_CODEBOX_PREPARED_DEPENDENCY_CACHE_DIR:-}"

    if [ -z "$base_dir" ]; then
        if [ -n "${HOMEBOY_CACHE_DIR:-}" ]; then
            base_dir="${HOMEBOY_CACHE_DIR%/}/prepared-bench-dependencies"
        elif [ -n "${HOME:-}" ]; then
            base_dir="${HOME%/}/.homeboy/cache/prepared-bench-dependencies"
        elif [ -n "$artifacts_dir" ]; then
            base_dir="${artifacts_dir%/}/prepared-bench-dependencies-cache"
        else
            base_dir="${TMPDIR:-/tmp}/homeboy-prepared-bench-dependencies"
        fi
    fi

    mkdir -p "$base_dir"
    printf '%s\n' "$base_dir"
}

_homeboy_prepared_dependency_cache_metadata() {
    local dependency_path="${1:-}"
    local prepare_root="${2:-}"
    local relative_plugin_path="${3:-}"
    local dependency_slug="${4:-}"
    local package_root="${5:-}"
    local mounted_plugin_dir="${6:-}"
    local php_version source_realpath prepare_realpath package_realpath git_state composer_json_hash composer_lock_hash catalog_entry_json node_engines_json composer_require_json

    source_realpath=$(cd "$dependency_path" && pwd -P)
    prepare_realpath=$(cd "$prepare_root" && pwd -P)
    if [ -n "$package_root" ] && [ -d "$package_root" ]; then
        package_realpath=$(cd "$package_root" && pwd -P)
    else
        package_realpath="$source_realpath"
    fi
    php_version=$(_homeboy_prepared_dependency_php_version)
    git_state=$(_homeboy_prepared_dependency_git_state "$prepare_root")
    composer_json_hash=$(_homeboy_sha256_file "${package_realpath}/composer.json")
    composer_lock_hash=$(_homeboy_sha256_file "${package_realpath}/composer.lock")
    catalog_entry_json=$(_homeboy_prepared_dependency_catalog_entry "$dependency_path" "$package_root")
    node_engines_json=$(_homeboy_package_engine_requirements "$package_realpath")
    composer_require_json=$(_homeboy_composer_runtime_requirements "$package_realpath")

    jq -n \
        --argjson catalogEntry "$catalog_entry_json" \
        --argjson nodeEngines "$node_engines_json" \
        --argjson composerRequire "$composer_require_json" \
        --arg schema 'homeboy/prepared-wordpress-bench-dependency/v1' \
        --arg slug "$dependency_slug" \
        --arg source_path "$source_realpath" \
        --arg prepare_root "$prepare_realpath" \
        --arg package_root "$package_realpath" \
        --arg relative_plugin_path "$relative_plugin_path" \
        --arg mounted_plugin_dir "$mounted_plugin_dir" \
        --arg git_state "$git_state" \
        --arg composer_json_hash "$composer_json_hash" \
        --arg composer_lock_hash "$composer_lock_hash" \
        --arg php_version "$php_version" \
        '($catalogEntry // {}) as $catalog |
        {
            schema: $schema,
            slug: ($catalog.slug // $slug),
            source_type: ($catalog.source_type // null),
            requested_version: ($catalog.requested_version // null),
            requested_revision: ($catalog.requested_revision // null),
            build_commands: ($catalog.build_commands // []),
            source_path: $source_path,
            prepare_root: $prepare_root,
            package_root: $package_root,
            relative_plugin_path: $relative_plugin_path,
            mounted_plugin_dir: $mounted_plugin_dir,
            plugin_file: ($catalog.plugin_file // null),
            activation_status: ($catalog.activation_status // "inactive"),
            runtime_requirements: {
                php: $php_version,
                node: $nodeEngines,
                composer: $composerRequire
            },
            git_state: $git_state,
            composer_json_hash: $composer_json_hash,
            composer_lock_hash: $composer_lock_hash,
            php_version: $php_version
        }'
}

_homeboy_prepared_dependency_cache_key() {
    local metadata_json="${1:-}"

    printf '%s' "$metadata_json" | jq -c '{source_path, prepare_root, package_root, relative_plugin_path, mounted_plugin_dir, git_state, composer_json_hash, composer_lock_hash, php_version}' | _homeboy_sha256_string
}

_homeboy_record_prepared_dependency_metadata() {
    local artifacts_dir="${1:-}"
    local metadata_json="${2:-}"
    local cache_key="${3:-}"
    local prepared_path="${4:-}"
    local cache_status="${5:-}"

    [ -n "$artifacts_dir" ] && [ -n "$metadata_json" ] || return 0
    command -v jq >/dev/null 2>&1 || return 0

    local metadata_file tmp_file existing_json
    metadata_file="${artifacts_dir%/}/prepared-bench-dependencies.json"
    mkdir -p "$(dirname "$metadata_file")"
    existing_json="[]"
    if [ -f "$metadata_file" ]; then
        existing_json=$(jq -c 'if type == "array" then . else [] end' "$metadata_file" 2>/dev/null || echo '[]')
    fi
    tmp_file=$(mktemp "${metadata_file}.XXXXXX")
    jq -n \
        --argjson existing "$existing_json" \
        --argjson dependency "$metadata_json" \
        --arg cache_key "$cache_key" \
        --arg prepared_path "$prepared_path" \
        --arg cache_status "$cache_status" \
        '$existing + [($dependency + {cache_key: $cache_key, prepared_path: $prepared_path, cache_status: $cache_status})]' > "$tmp_file"
    mv "$tmp_file" "$metadata_file"
}

homeboy_get_prepared_validation_dependency_slug() {
    local dependency_path="${1:-}"
    local artifacts_dir="${2:-}"

    [ -n "$dependency_path" ] && [ -n "$artifacts_dir" ] || return 1
    command -v jq >/dev/null 2>&1 || return 1

    local metadata_file dependency_realpath
    metadata_file="${artifacts_dir%/}/prepared-bench-dependencies.json"
    [ -f "$metadata_file" ] || return 1
    dependency_realpath=$(cd "$dependency_path" && pwd -P 2>/dev/null || printf '%s\n' "$dependency_path")

    jq -er --arg dependencyPath "$dependency_path" --arg dependencyRealpath "$dependency_realpath" '
        if type == "array" then . else [] end
        | map(select(.prepared_path == $dependencyPath or .prepared_path == $dependencyRealpath or .package_root == $dependencyPath or .package_root == $dependencyRealpath))
        | .[-1].slug // empty
    ' "$metadata_file" 2>/dev/null
}

homeboy_export_wordpress_dependencies_json() {
    local dependency_paths="${1:-}"
    local artifacts_dir="${2:-}"

    command -v jq >/dev/null 2>&1 || return 0

    local metadata_file dependencies_json
    metadata_file="${artifacts_dir%/}/prepared-bench-dependencies.json"
    if [ -n "$artifacts_dir" ] && [ -f "$metadata_file" ]; then
        dependencies_json=$(jq -c '
            if type == "array" then . else [] end
            | map({
                slug,
                path: (.prepared_path // .package_root // .source_path),
                local_path: (.source_path // .package_root // .prepared_path),
                runner_path: (.mounted_plugin_dir // null),
                source: (.source_type // .cache_status // null),
                source_type: (.source_type // null),
                ref: (.requested_revision // null),
                plugin_file: (.plugin_file // null),
                resolved_by: (if .cache_status then "wp-codebox-bench-prepare" else "wordpress-dependency-resolution" end)
            })
        ' "$metadata_file" 2>/dev/null || printf '[]\n')
        export HOMEBOY_WORDPRESS_DEPENDENCIES_JSON="$dependencies_json"
        return 0
    fi

    local catalog_json="${HOMEBOY_WORDPRESS_DEPENDENCY_CATALOG_JSON:-${_HOMEBOY_RESOLVED_DEPENDENCY_CATALOG_JSON:-[]}}"
    if printf '%s' "$catalog_json" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
        dependencies_json=$(printf '%s' "$catalog_json" | jq -c '
            map({
                slug,
                path: .resolved_path,
                local_path: .resolved_path,
                runner_path: (if .slug then "/wordpress/wp-content/plugins/" + .slug else null end),
                source: (.source_type // null),
                source_type: (.source_type // null),
                ref: (.requested_revision // null),
                plugin_file: (.plugin_file // null),
                resolved_by: "wordpress-dependency-resolution"
            })
        ' 2>/dev/null || printf '[]\n')
        export HOMEBOY_WORDPRESS_DEPENDENCIES_JSON="$dependencies_json"
        return 0
    fi

    local entries="[]" dependency_path dependency_slug plugin_file plugin_relative_file
    while IFS= read -r dependency_path; do
        [ -n "$dependency_path" ] || continue
        [ -d "$dependency_path" ] || continue
        dependency_slug=$(homeboy_get_validation_dependency_slug "$dependency_path" || basename "$dependency_path")
        plugin_file=""
        plugin_relative_file=""
        plugin_file=$(homeboy_find_validation_dependency_plugin_main_file "$dependency_path" || true)
        if [ -n "$plugin_file" ]; then
            plugin_relative_file="${dependency_slug}/${plugin_file#"${dependency_path%/}/"}"
        fi
        entries=$(jq -nc \
            --argjson entries "$entries" \
            --arg slug "$dependency_slug" \
            --arg path "$dependency_path" \
            --arg pluginFile "$plugin_relative_file" \
            '$entries + [{
                slug: $slug,
                path: $path,
                local_path: $path,
                runner_path: "/wordpress/wp-content/plugins/" + $slug,
                source: "path",
                source_type: null,
                ref: null,
                plugin_file: (if $pluginFile == "" then null else $pluginFile end),
                resolved_by: "wordpress-dependency-paths"
            }]')
    done <<< "$dependency_paths"

    export HOMEBOY_WORDPRESS_DEPENDENCIES_JSON="$entries"
}

_homeboy_command_version() {
    local command_name="${1:-}"

    [ -n "$command_name" ] || return 0
    command -v "$command_name" >/dev/null 2>&1 || return 0
    "$command_name" --version 2>/dev/null | head -1 || true
}

_homeboy_package_engine_requirements() {
    local package_path="${1:-}"

    [ -n "$package_path" ] && [ -f "${package_path%/}/package.json" ] || {
        printf '{}\n'
        return 0
    }

    jq -c '.engines // {}' "${package_path%/}/package.json" 2>/dev/null || printf '{}\n'
}

_homeboy_tail_file() {
    local file_path="${1:-}"
    local byte_limit="${2:-4000}"

    [ -n "$file_path" ] && [ -f "$file_path" ] || return 0
    tail -c "$byte_limit" "$file_path" 2>/dev/null || true
}

_homeboy_record_bench_dependency_build_failure() {
    local artifacts_dir="${1:-}"
    local dependency_slug="${2:-}"
    local dependency_path="${3:-}"
    local package_root="${4:-}"
    local prepare_root="${5:-}"
    local package_path="${6:-}"
    local attempted_command="${7:-}"
    local exit_code="${8:-}"
    local output_file="${9:-}"

    [ -n "$artifacts_dir" ] || return 0
    command -v jq >/dev/null 2>&1 || return 0

    local failures_file diagnostics_file tmp_file existing_json stderr_tail node_version npm_version engines_json
    failures_file="${artifacts_dir%/}/failed-bench-dependencies.json"
    diagnostics_file="${artifacts_dir%/}/wordpress-dependency-build-diagnostics.json"
    mkdir -p "$artifacts_dir"
    existing_json="[]"
    if [ -f "$failures_file" ]; then
        existing_json=$(jq -c 'if type == "array" then . else [] end' "$failures_file" 2>/dev/null || echo '[]')
    fi
    stderr_tail=$(_homeboy_tail_file "$output_file" 4000)
    node_version=$(_homeboy_command_version node)
    npm_version=$(_homeboy_command_version npm)
    engines_json=$(_homeboy_package_engine_requirements "$package_path")

    tmp_file=$(mktemp "${failures_file}.XXXXXX")
    jq -n \
        --argjson existing "$existing_json" \
        --arg schema 'homeboy/wordpress-bench-dependency-build-failure/v1' \
        --arg code 'wordpress-bench-dependency-build-failed' \
        --arg slug "$dependency_slug" \
        --arg dependencyPath "$dependency_path" \
        --arg packageRoot "$package_root" \
        --arg prepareRoot "$prepare_root" \
        --arg packagePath "$package_path" \
        --arg attemptedCommand "$attempted_command" \
        --arg exitCode "$exit_code" \
        --arg nodeVersion "$node_version" \
        --arg npmVersion "$npm_version" \
        --argjson engineRequirements "$engines_json" \
        --arg stderrTail "$stderr_tail" \
        '$existing + [{
            schema: $schema,
            code: $code,
            severity: "error",
            phase: "dependency-build",
            dependency_slug: (if $slug == "" then null else $slug end),
            dependency_path: (if $dependencyPath == "" then null else $dependencyPath end),
            package_root: (if $packageRoot == "" then null else $packageRoot end),
            prepare_root: (if $prepareRoot == "" then null else $prepareRoot end),
            package_path: (if $packagePath == "" then null else $packagePath end),
            node_version: (if $nodeVersion == "" then null else $nodeVersion end),
            npm_version: (if $npmVersion == "" then null else $npmVersion end),
            engine_requirements: $engineRequirements,
            attempted_command: (if $attemptedCommand == "" then null else $attemptedCommand end),
            exit_code: (if $exitCode == "" then null else ($exitCode | tonumber) end),
            stderr_tail: (if $stderrTail == "" then null else $stderrTail end)
        }]' > "$tmp_file"
    mv "$tmp_file" "$failures_file"

    jq -n \
        --arg schema 'homeboy/wordpress-bench-diagnostic/v1' \
        --slurpfile failures "$failures_file" \
        '{schema: $schema, diagnostics: ($failures[0] // [])}' > "$diagnostics_file"
}

homeboy_prepare_validation_dependency_for_wp_codebox_bench() {
    local dependency_path="${1:-}"
    local artifacts_dir="${2:-}"

    [ -n "$dependency_path" ] && [ -d "$dependency_path" ] || return 1

    local dependency_slug package_root catalog_entry_json catalog_slug
    dependency_slug=$(homeboy_get_validation_dependency_slug "$dependency_path" || basename "$dependency_path")
    package_root=$(homeboy_find_validation_dependency_plugin_package_root "$dependency_path" "$dependency_slug" || true)
    [ -n "$package_root" ] && [ -d "$package_root" ] || package_root="$dependency_path"
    catalog_entry_json=$(_homeboy_prepared_dependency_catalog_entry "$dependency_path" "$package_root")
    catalog_slug=$(printf '%s' "$catalog_entry_json" | jq -r '.slug // empty' 2>/dev/null || true)
    [ -n "$catalog_slug" ] && dependency_slug="$catalog_slug"

    if ! homeboy_dependency_needs_composer_prepare "$package_root"; then
        local source_realpath package_realpath relative_source_plugin_path metadata_json
        source_realpath=$(cd "$dependency_path" && pwd -P)
        package_realpath=$(cd "$package_root" && pwd -P)
        relative_source_plugin_path=""
        if [ "$package_realpath" != "$source_realpath" ]; then
            relative_source_plugin_path="${package_realpath#"$source_realpath"/}"
        fi
        metadata_json=$(_homeboy_prepared_dependency_cache_metadata "$dependency_path" "$dependency_path" "$relative_source_plugin_path" "$dependency_slug" "$package_root" "/wordpress/wp-content/plugins/${dependency_slug}")
        _homeboy_record_prepared_dependency_metadata "$artifacts_dir" "$metadata_json" "source-package-root" "$package_root" "source"
        printf '%s\n' "$package_root"
        return 0
    fi

    if ! command -v composer >/dev/null 2>&1; then
        _homeboy_record_bench_dependency_build_failure "$artifacts_dir" "$dependency_slug" "$dependency_path" "$package_root" "$package_root" "$package_root" 'composer install --no-dev --no-interaction --no-progress --prefer-dist --classmap-authoritative --no-plugins' 127 ''
        echo "Error: WordPress bench dependency '${dependency_path}' has composer.json but no vendor autoload files, and composer is not available." >&2
        return 1
    fi

    local prepare_root
    prepare_root="$(_homeboy_dependency_repo_root "$package_root")"
    [ -n "$prepare_root" ] && [ -d "$prepare_root" ] || prepare_root="$dependency_path"

    local dependency_realpath prepare_realpath relative_plugin_path
    dependency_realpath=$(cd "$dependency_path" && pwd -P)
    prepare_realpath=$(cd "$prepare_root" && pwd -P)
    case "$dependency_realpath" in
        "$prepare_realpath")
            relative_plugin_path=""
            ;;
        "$prepare_realpath"/*)
            relative_plugin_path="${dependency_realpath#"$prepare_realpath"/}"
            ;;
        *)
            prepare_root="$dependency_path"
            prepare_realpath="$dependency_realpath"
            relative_plugin_path=""
            ;;
    esac

    local cache_metadata cache_key cache_dir cache_entry prepared_root prepared_plugin_path metadata_file
    cache_metadata=$(_homeboy_prepared_dependency_cache_metadata "$dependency_path" "$prepare_root" "$relative_plugin_path" "$dependency_slug" "$package_root" "/wordpress/wp-content/plugins/${dependency_slug}")
    cache_key=$(_homeboy_prepared_dependency_cache_key "$cache_metadata")
    cache_dir=$(_homeboy_prepared_dependency_cache_dir "$artifacts_dir")
    cache_entry="${cache_dir%/}/${dependency_slug}-${cache_key}"
    prepared_root="${cache_entry}/root"
    metadata_file="${cache_entry}/metadata.json"

    prepared_plugin_path="$prepared_root"
    if [ -n "$relative_plugin_path" ]; then
        prepared_plugin_path="${prepared_root}/${relative_plugin_path}"
    fi

    if [ -f "$metadata_file" ] && [ -d "$prepared_plugin_path" ] && { [ -f "${prepared_plugin_path}/vendor/autoload.php" ] || [ -f "${prepared_plugin_path}/vendor/autoload_packages.php" ]; }; then
        echo "Using cached WordPress bench dependency '${dependency_slug}' at ${prepared_plugin_path}" >&2
        _homeboy_record_prepared_dependency_metadata "$artifacts_dir" "$cache_metadata" "$cache_key" "$prepared_plugin_path" "hit"
        printf '%s\n' "$prepared_plugin_path"
        return 0
    fi

    echo "Preparing WordPress bench dependency '${dependency_slug}' cache miss ${cache_key}" >&2

    local tmp_entry tmp_prepared_root tmp_prepared_plugin_path
    tmp_entry=$(mktemp -d "${cache_dir%/}/.${dependency_slug}-${cache_key}.XXXXXX")
    tmp_prepared_root="${tmp_entry}/root"
    _homeboy_copy_dependency_prepare_root "$prepare_root" "$tmp_prepared_root"

    tmp_prepared_plugin_path="$tmp_prepared_root"
    if [ -n "$relative_plugin_path" ]; then
        tmp_prepared_plugin_path="${tmp_prepared_root}/${relative_plugin_path}"
    fi

    if [ ! -f "${tmp_prepared_plugin_path}/composer.json" ]; then
        rm -rf "$tmp_entry"
        echo "Error: Prepared WordPress bench dependency '${dependency_path}' lost composer.json at '${tmp_prepared_plugin_path}'." >&2
        return 1
    fi

    echo "Preparing WordPress bench dependency '${dependency_slug}' with Composer at ${tmp_prepared_plugin_path}" >&2
    local composer_output composer_exit
    composer_output=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-bench-dependency-composer-${dependency_slug}.XXXXXX")
    set +e
    # `--no-plugins`: see _homeboy_install_git_dependency_composer_packages().
    # The prepared copy only needs an autoloader, so Composer plugins must not
    # make preparation depend on the host's `allow-plugins` policy.
    composer install --working-dir="$tmp_prepared_plugin_path" --no-dev --no-interaction --no-progress --prefer-dist --classmap-authoritative --no-plugins >"$composer_output" 2>&1
    composer_exit=$?
    set -e
    if [ "$composer_exit" -ne 0 ]; then
        cat "$composer_output" >&2
        _homeboy_record_bench_dependency_build_failure "$artifacts_dir" "$dependency_slug" "$dependency_path" "$package_root" "$prepare_root" "$tmp_prepared_plugin_path" "composer install --working-dir=${tmp_prepared_plugin_path} --no-dev --no-interaction --no-progress --prefer-dist --classmap-authoritative --no-plugins" "$composer_exit" "$composer_output"
        rm -rf "$tmp_entry"
        rm -f "$composer_output"
        echo "Error: Could not prepare WordPress bench dependency '${dependency_slug}' with Composer at ${tmp_prepared_plugin_path}." >&2
        return 1
    fi
    rm -f "$composer_output"

    if [ ! -f "${tmp_prepared_plugin_path}/vendor/autoload.php" ] && [ ! -f "${tmp_prepared_plugin_path}/vendor/autoload_packages.php" ]; then
        rm -rf "$tmp_entry"
        echo "Error: Composer preparation for WordPress bench dependency '${dependency_slug}' did not create vendor autoload files at ${tmp_prepared_plugin_path}." >&2
        return 1
    fi

    if [ -f "$metadata_file" ] && [ -d "$prepared_plugin_path" ] && { [ -f "${prepared_plugin_path}/vendor/autoload.php" ] || [ -f "${prepared_plugin_path}/vendor/autoload_packages.php" ]; }; then
        rm -rf "$tmp_entry"
        echo "Using cached WordPress bench dependency '${dependency_slug}' at ${prepared_plugin_path}" >&2
        _homeboy_record_prepared_dependency_metadata "$artifacts_dir" "$cache_metadata" "$cache_key" "$prepared_plugin_path" "hit"
        printf '%s\n' "$prepared_plugin_path"
        return 0
    fi

    printf '%s\n' "$cache_metadata" > "${tmp_entry}/metadata.json"
    rm -rf "$cache_entry"
    mv "$tmp_entry" "$cache_entry"

    _homeboy_record_prepared_dependency_metadata "$artifacts_dir" "$cache_metadata" "$cache_key" "$prepared_plugin_path" "miss"

    printf '%s\n' "$prepared_plugin_path"
}

homeboy_prepare_validation_dependency_paths_for_wp_codebox_bench() {
    local dependency_paths="${1:-}"
    local artifacts_dir="${2:-}"

    [ -n "$dependency_paths" ] || return 0
    [ -n "$artifacts_dir" ] || return 1

    local dependency_path prepared_path
    while IFS= read -r dependency_path; do
        [ -n "$dependency_path" ] || continue
        [ -d "$dependency_path" ] || continue
        if prepared_path=$(homeboy_prepare_validation_dependency_for_wp_codebox_bench "$dependency_path" "$artifacts_dir"); then
            [ -n "$prepared_path" ] && printf '%s\n' "$prepared_path"
        else
            echo "Warning: WordPress bench dependency provider skipped failed dependency: ${dependency_path}" >&2
        fi
    done <<< "$dependency_paths"
}

homeboy_prepare_validation_dependency_for_wp_codebox_runtime() {
    homeboy_prepare_validation_dependency_for_wp_codebox_bench "$@"
}

homeboy_prepare_validation_dependency_paths_for_wp_codebox_runtime() {
    local dependency_paths="${1:-}"
    local artifacts_dir="${2:-}"
    local context="${3:-wordpress}"

    [ -n "$dependency_paths" ] || return 0
    [ -n "$artifacts_dir" ] || return 1

    local dependency_path prepared_path failed
    failed=0
    while IFS= read -r dependency_path; do
        [ -n "$dependency_path" ] || continue
        [ -d "$dependency_path" ] || continue
        if prepared_path=$(homeboy_prepare_validation_dependency_for_wp_codebox_runtime "$dependency_path" "$artifacts_dir"); then
            [ -n "$prepared_path" ] && printf '%s\n' "$prepared_path"
        else
            echo "Error: WordPress ${context} dependency provider could not prepare runtime-complete plugin input: ${dependency_path}" >&2
            failed=1
        fi
    done <<< "$dependency_paths"

    [ "$failed" -eq 0 ]
}

homeboy_export_validation_dependency_paths() {
    local plugin_path="${1:-}"
    local existing_paths
    existing_paths=$(homeboy_translate_validation_dependency_paths "${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}")
    local resolved_paths
    resolved_paths=$(HOMEBOY_SUPPRESS_DEPENDENCY_RESOLUTION_LOG=1 homeboy_resolve_validation_dependency_paths "$plugin_path" || true)
    resolved_paths=$(homeboy_translate_validation_dependency_paths "$resolved_paths")

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
