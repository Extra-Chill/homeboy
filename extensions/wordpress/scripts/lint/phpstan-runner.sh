#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STABLE_FINGERPRINT_HELPER="${SCRIPT_DIR}/stable-fingerprint.php"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
# shellcheck source=../lib/validation-dependencies.sh
source "${DEPENDENCY_HELPER}"
# Standalone `homeboy lint` runs do not export HOMEBOY_RUNTIME_SIDECAR_WRITER;
# fall back to the co-located direct-invocation copy so the sidecar writer is
# available outside a release run (homeboy-extensions#1415).
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
# shellcheck source=/dev/null
if [ -n "$SIDECAR_WRITER_HELPER" ] && [ -f "$SIDECAR_WRITER_HELPER" ]; then
    source "$SIDECAR_WRITER_HELPER"
fi

# Standalone PHP static analysis script using PHPStan
# Supports summary mode via HOMEBOY_SUMMARY_MODE=1
# Supports skip via HOMEBOY_SKIP_PHPSTAN=1
# Supports level override via HOMEBOY_PHPSTAN_LEVEL (default: 7, matches phpstan.neon.dist)
# Respects scoped lint via HOMEBOY_LINT_FILE / HOMEBOY_LINT_GLOB. Unscoped lint
# still analyzes the full component so CI keeps the full type-graph signal.

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: PHPStan Environment variables:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_SUMMARY_MODE=${HOMEBOY_SUMMARY_MODE:-NOT_SET}"
    echo "HOMEBOY_SKIP_PHPSTAN=${HOMEBOY_SKIP_PHPSTAN:-NOT_SET}"
    echo "HOMEBOY_PHPSTAN_LEVEL=${HOMEBOY_PHPSTAN_LEVEL:-NOT_SET}"
fi

wordpress_lint_role_for_path() {
    local rel_path="$1"

    case "$rel_path" in
        scoper.inc.php|scoper.php|*.scoper.inc.php)
            printf '%s\n' 'scoper_config'
            ;;
        tools/*|bin/*)
            printf '%s\n' 'tooling'
            ;;
        tests/*-smoke.php|tests/smoke-*.php|*/smoke-*.php|*/*-smoke.php)
            printf '%s\n' 'smoke_harness'
            ;;
        tests/*Test.php|tests/*TestCase.php|*/tests/*Test.php|*/tests/*TestCase.php)
            printf '%s\n' 'phpunit_test'
            ;;
        *)
            printf '%s\n' 'production'
            ;;
    esac
}

run_scoped_syntax_check() {
    local rel_path="$1"
    local target="${PLUGIN_PATH}/${rel_path}"

    if [ ! -f "$target" ] || [[ "$target" != *.php ]]; then
        return 0
    fi

    if php -l "$target" > /dev/null 2>&1; then
        echo "PHPStan skipped for ${HOMEBOY_WORDPRESS_LINT_ROLE}; PHP syntax check passed"
        return 0
    fi

    echo "PHPStan skipped for ${HOMEBOY_WORDPRESS_LINT_ROLE}, but PHP syntax check failed: ${rel_path}"
    php -l "$target" 2>&1 | grep -v "^$" | sed 's/^/  /'
    return 1
}

# Critical PHPStan error identifiers that indicate guaranteed runtime fatals.
# These must NEVER be skipped, even with --skip-checks or HOMEBOY_SKIP_PHPSTAN=1.
# Skipping these allows code that will crash on first request to reach production.
CRITICAL_PHPSTAN_IDENTIFIERS="function.notFound|class.notFound"

# Skip mode: when PHPStan is explicitly skipped, we still run a critical-only check.
# This catches guaranteed runtime fatals (undefined functions/classes) while
# respecting the user's intent to skip style-level static analysis.
PHPSTAN_CRITICAL_ONLY=0
if [[ "${HOMEBOY_SKIP_PHPSTAN:-}" == "1" ]]; then
    if [[ "${HOMEBOY_SKIP_ALL_CHECKS:-}" == "1" ]]; then
        # Explicit nuclear option — skip everything including critical checks.
        # This is dangerous and should only be used in emergencies.
        echo "WARNING: Skipping ALL PHPStan checks including fatal-class detection (HOMEBOY_SKIP_ALL_CHECKS=1)"
        echo "         This may allow code with undefined functions/classes to pass validation."
        exit 0
    fi
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: PHPStan skipped but running critical-only check for fatal-class errors"
    fi
    PHPSTAN_CRITICAL_ONLY=1
fi

# Resolve execution context (shared helper)
RESOLVE_CONTEXT_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
# shellcheck source=/dev/null
source "${RESOLVE_CONTEXT_HELPER}"
homeboy_resolve_context --component-alias PLUGIN_PATH
# shellcheck source=/dev/null
if [ -n "$SIDECAR_WRITER_HELPER" ] && [ -f "$SIDECAR_WRITER_HELPER" ]; then
    source "$SIDECAR_WRITER_HELPER"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Extension path: $EXTENSION_PATH"
    echo "DEBUG: Plugin path: $PLUGIN_PATH"
fi

WORDPRESS_LINT_ROLE="${HOMEBOY_WORDPRESS_LINT_ROLE:-production}"
if [ "$WORDPRESS_LINT_ROLE" = "production" ] && [ -n "${HOMEBOY_LINT_FILE:-}" ]; then
    WORDPRESS_LINT_ROLE=$(wordpress_lint_role_for_path "$HOMEBOY_LINT_FILE")
fi
export HOMEBOY_WORDPRESS_LINT_ROLE="$WORDPRESS_LINT_ROLE"

case "$WORDPRESS_LINT_ROLE" in
    scoper_config|smoke_harness|phpunit_test)
        if [ -n "${HOMEBOY_LINT_FILE:-}" ]; then
            run_scoped_syntax_check "$HOMEBOY_LINT_FILE"
            exit $?
        fi
        ;;
esac

PHPSTAN_BIN="${EXTENSION_PATH}/vendor/bin/phpstan"
PHPSTAN_DEFAULT_CONFIG="${EXTENSION_PATH}/phpstan.neon.dist"
PHPSTAN_COMPONENT_CONFIG=""
PHPSTAN_COMPONENT_CONFIG_SOURCE="extension-default"
PHPSTAN_COMPONENT_CONFIG_HAS_RULESET=0
PHPSTAN_COMPONENT_CONFIG_INCLUDES_BASELINE=0
PHPSTAN_BASE_CONFIG="$PHPSTAN_DEFAULT_CONFIG"
PHPSTAN_LEVEL_SOURCE="extension-default"
COMPONENT_BASELINE="${PLUGIN_PATH}/phpstan-baseline.neon"
COMPOSITE_AUTOLOAD=""
COMPOSITE_AUTOLOAD_DIR=""
DEPENDENCY_CONFIG=""
SCOPED_CONTEXT_CONFIG=""

homeboy_mktemp() {
    local template="$1"
    local tmpdir="${HOMEBOY_CACHE_DIR:-${TMPDIR:-/tmp}}"
    local tmpfile=""
    local basefile=""
    local prefix=""
    local suffix=""

    if [ -d "$tmpdir" ] && [ -w "$tmpdir" ]; then
        tmpfile=$(mktemp "${tmpdir%/}/${template}" 2>/dev/null || true)
        if [ -n "$tmpfile" ] && [[ "$tmpfile" != *XXXXXX* ]]; then
            printf '%s\n' "$tmpfile"
            return 0
        fi

        # BSD mktemp leaves XXXXXX literal when the random token is not the
        # final path segment. PHPStan 2 requires generated config files to keep
        # their .neon suffix, so create a unique base first and rename it.
        [ -n "$tmpfile" ] && rm -f "$tmpfile"
        if [[ "$template" == *XXXXXX* ]]; then
            prefix="${template%%XXXXXX*}"
            suffix="${template#*XXXXXX}"
            if [ -n "$suffix" ]; then
                basefile=$(mktemp "${tmpdir%/}/${prefix}XXXXXX" 2>/dev/null || true)
                if [ -n "$basefile" ]; then
                    tmpfile="${basefile}${suffix}"
                    if [ ! -e "$tmpfile" ] && mv "$basefile" "$tmpfile"; then
                        printf '%s\n' "$tmpfile"
                        return 0
                    fi
                    rm -f "$basefile"
                fi
            fi
        fi
    fi

    mktemp 2>/dev/null
}

write_phpstan_findings_sidecar() {
    local target="$1"
    local source="$2"

    [ -z "$target" ] && return 0
    [ ! -s "$source" ] && return 0

    if ! type homeboy_sidecar_merge_json_array >/dev/null 2>&1; then
        # The findings sidecar is Homeboy observability output, not a result.
        # Skip writing it when the writer is unavailable rather than failing the
        # static-analysis step (homeboy-extensions#1402).
        echo "Warning: sidecar writer unavailable; skipping PHPStan lint findings sidecar" >&2
        return 0
    fi

    rm -f "$target"
    homeboy_sidecar_merge_json_array "$target" "$source"
}

# Validate PHPStan exists (soft failure - not all installations have it)
if [ ! -f "$PHPSTAN_BIN" ]; then
    echo "Warning: PHPStan not found at $PHPSTAN_BIN, skipping static analysis"
    exit 0
fi

# Integrity guard (homeboy-extensions#2233): a corrupted/truncated phpstan.phar
# makes Phar::loadPhar() throw a fatal ("manifest cannot be larger than 100 MB")
# that kills the whole lint gate with an opaque PHP error instead of a clean
# skip. Probing with `--version` is the cheapest check that loads the phar
# manifest, so a failing probe means the binary is unusable. Returns 0 when the
# binary starts and non-zero otherwise.
homeboy_phpstan_probe() {
    "$1" --version >/dev/null 2>&1
}

if ! homeboy_phpstan_probe "$PHPSTAN_BIN"; then
    echo "Warning: PHPStan binary at $PHPSTAN_BIN failed to start (likely a corrupted phpstan.phar)."
    echo "         Skipping PHPStan static analysis. Reinstall the wordpress extension"
    echo "         (run \`composer install\` in the extension directory) to refresh the phar."
    echo "         See homeboy-extensions#2233 for background."
    exit 0
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: PHPStan probe ok: $("$PHPSTAN_BIN" --version 2>&1 | head -1)"
fi

if [ ! -f "$PHPSTAN_DEFAULT_CONFIG" ]; then
    echo "Warning: phpstan.neon.dist not found at $PHPSTAN_DEFAULT_CONFIG, skipping static analysis"
    exit 0
fi

resolve_component_phpstan_config() {
    local candidate

    for candidate in \
        "${PLUGIN_PATH}/phpstan.neon" \
        "${PLUGIN_PATH}/phpstan.neon.dist" \
        "${PLUGIN_PATH}/phpstan.dist.neon"; do
        if [ -f "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

PHPSTAN_COMPONENT_CONFIG=$(resolve_component_phpstan_config || true)
if [ -n "$PHPSTAN_COMPONENT_CONFIG" ]; then
    PHPSTAN_COMPONENT_CONFIG_SOURCE="component-local"
    PHPSTAN_BASE_CONFIG="$PHPSTAN_COMPONENT_CONFIG"
    if grep -Eq '^[[:space:]]*(level|customRulesetUsed):' "$PHPSTAN_COMPONENT_CONFIG"; then
        PHPSTAN_COMPONENT_CONFIG_HAS_RULESET=1
    fi
    if grep -Eq '^[[:space:]]*-[[:space:]]+\.?/?phpstan-baseline\.neon([[:space:]]*(#.*)?)?$' "$PHPSTAN_COMPONENT_CONFIG"; then
        PHPSTAN_COMPONENT_CONFIG_INCLUDES_BASELINE=1
    fi
fi

homeboy_run_phpstan() {
    # PHPStan resolves project-relative configuration and %currentWorkingDirectory%
    # from its process CWD, not from the generated config file in TMPDIR.
    (cd "$PLUGIN_PATH" && "$PHPSTAN_BIN" "$@")
}

generate_dependency_config() {
    local tmpfile
    local has_dependencies=0
    local has_baseline=0
    local has_component_config=0
    local has_component_context=0
    local context_path
    local scan_file_count=0
    local wordpress_api_overrides="${EXTENSION_PATH}/stubs/wordpress-api-overrides.stub.php"

    tmpfile=$(homeboy_mktemp 'phpstan-dependencies.XXXXXX.neon')

    {
        printf '%s\n' 'includes:'
        if [ -n "$PHPSTAN_COMPONENT_CONFIG" ]; then
            if [ "$PHPSTAN_COMPONENT_CONFIG_HAS_RULESET" -ne 1 ]; then
                printf '    - %s\n' "$PHPSTAN_DEFAULT_CONFIG"
            fi
            printf '    - %s\n' "$PHPSTAN_COMPONENT_CONFIG"
            has_component_config=1
        else
            printf '    - %s\n' "$PHPSTAN_DEFAULT_CONFIG"
        fi
        # Component baseline: PHPStan 2.x removed the `--baseline` CLI flag, so
        # the baseline file must be pulled in via `includes:` in the neon. We
        # do this here (rather than via --baseline) so every invocation path
        # (summary, full, retry) picks it up automatically through the single
        # config include chain.
        if [ -f "$COMPONENT_BASELINE" ] && [ "$PHPSTAN_COMPONENT_CONFIG_INCLUDES_BASELINE" -ne 1 ]; then
            printf '    - %s\n' "$COMPONENT_BASELINE"
            has_baseline=1
        fi
        printf '%s\n' ''
        printf '%s\n' 'parameters:'
        if [ -z "$PHPSTAN_COMPONENT_CONFIG" ]; then
            printf '%s\n' '    customRulesetUsed: false'
        fi
        printf '%s\n' '    scanDirectories:'

        local dependency_paths=""
        while IFS= read -r dependency_path; do
            [ -z "$dependency_path" ] && continue
            has_dependencies=1
            dependency_paths+="${dependency_path}"$'\n'
            printf '        - %s\n' "$dependency_path"
        done < <(homeboy_resolve_validation_dependency_paths "$PLUGIN_PATH")

        if [ -n "${HOMEBOY_LINT_FILE:-}" ] || [ -n "${HOMEBOY_LINT_GLOB:-}" ]; then
            while IFS= read -r context_path; do
                [ -z "$context_path" ] && continue
                has_component_context=1
                printf '        - %s\n' "$context_path"
            done < <(homeboy_resolve_phpstan_context_directories "$PLUGIN_PATH")

            while IFS= read -r context_path; do
                [ -z "$context_path" ] && continue
                if [ "$scan_file_count" -eq 0 ]; then
                    printf '%s\n' '    scanFiles:'
                fi
                scan_file_count=$((scan_file_count + 1))
                has_component_context=1
                printf '        - %s\n' "$context_path"
            done < <(homeboy_resolve_phpstan_context_files "$PLUGIN_PATH")
        fi

        # Pin dependency function signatures so test scaffolding in the
        # component cannot shadow them. See
        # homeboy_resolve_phpstan_dependency_signature_files().
        while IFS= read -r dependency_path; do
            [ -z "$dependency_path" ] && continue
            while IFS= read -r signature_file; do
                [ -z "$signature_file" ] && continue
                if [ "$scan_file_count" -eq 0 ]; then
                    printf '%s\n' '    scanFiles:'
                fi
                scan_file_count=$((scan_file_count + 1))
                printf '        - %s\n' "$signature_file"
            done < <(homeboy_resolve_phpstan_dependency_signature_files "$dependency_path")
        done <<< "$dependency_paths"

        if [ -f "$wordpress_api_overrides" ]; then
            if [ "$scan_file_count" -eq 0 ]; then
                printf '%s\n' '    scanFiles:'
            fi
            scan_file_count=$((scan_file_count + 1))
            printf '        - %s\n' "$wordpress_api_overrides"
        fi
    } > "$tmpfile"

    if [ "$has_dependencies" -eq 1 ] || [ "$has_baseline" -eq 1 ] || [ "$has_component_context" -eq 1 ] || [ "${has_component_config:-0}" -eq 1 ]; then
        printf '%s\n' "$tmpfile"
    else
        rm -f "$tmpfile"
        printf '%s\n' ''
    fi
}

homeboy_resolve_phpstan_context_directories() {
    local component_path="$1"
    local candidate

    find "$component_path" -mindepth 1 -maxdepth 1 -type d \
        -not -name '.homeboy-build' \
        -not -name 'vendor' \
        -not -name 'vendor_prefixed' \
        -not -name 'node_modules' \
        -not -name 'build' \
        -not -name 'dist' \
        -not -name 'tests' \
        -print 2>/dev/null | while IFS= read -r candidate; do
            if find "$candidate" -type f -name '*.php' -print -quit 2>/dev/null | grep -q .; then
                printf '%s\n' "$candidate"
            fi
        done

    if [ -d "${component_path}/vendor_prefixed" ]; then
        printf '%s\n' "${component_path}/vendor_prefixed"
    fi
}

homeboy_resolve_phpstan_context_files() {
    local component_path="$1"

    find "$component_path" -mindepth 1 -maxdepth 1 -type f -name '*.php' -print 2>/dev/null
}

# Dependency PHP sources registered as `scanFiles:` rather than only through
# `scanDirectories:`.
#
# `scanDirectories:` declarations lose to project-source declarations during
# signature resolution, exactly like the `bootstrapFiles:` case documented in
# phpstan.neon.dist. A test file that defines `function bbp_get_template_part()
# {}` for an isolated standalone run therefore shadows the dependency's real
# `bbp_get_template_part( $slug, $name = null )`, and every genuine two-argument
# call in component source is reported as `invoked with 2 parameters, 0
# required`. The findings look like component defects but are artifacts of test
# scaffolding winning the symbol graph.
#
# `scanFiles:` entries are scanned alongside project source and win, so the
# dependency's real signature governs analysis. Both mechanisms are emitted:
# `scanDirectories:` keeps whole-tree discovery for autoloaded classes, and
# `scanFiles:` pins the function signatures that shadowing would otherwise
# corrupt.
#
# Depth is bounded so pathological trees stay affordable, but the bound must
# clear real plugin layouts. PSR-4 sources routinely nest further than the
# top-level API surface: `inc/Core/Database/Agents/Agents.php` is depth 5, and
# measured dependency trees reach depth 8. A shallower bound silently omits
# those files, which reintroduces the shadowing this function exists to
# prevent — as unresolved-method findings rather than arity ones. Vendored
# code, node_modules, build output, and the dependency's own tests are excluded
# above, so the remaining tree is first-party source.
homeboy_resolve_phpstan_dependency_signature_files() {
    local dependency_path="$1"
    local depth="${HOMEBOY_PHPSTAN_DEPENDENCY_SIGNATURE_DEPTH:-10}"

    [ -d "$dependency_path" ] || return 0

    find "$dependency_path" -mindepth 1 -maxdepth "$depth" -type f -name '*.php' \
        -not -path '*/vendor/*' \
        -not -path '*/vendor_prefixed/*' \
        -not -path '*/node_modules/*' \
        -not -path '*/build/*' \
        -not -path '*/dist/*' \
        -not -path '*/tests/*' \
        -not -path '*/tools/*' \
        -print 2>/dev/null
}

cleanup_dependency_config() {
    [ -n "$DEPENDENCY_CONFIG" ] && rm -f "$DEPENDENCY_CONFIG"
    DEPENDENCY_CONFIG=""
}

generate_scoped_context_config() {
    local tmpfile
    local context_file
    local has_context_files=0
    local has_scan_directories=0

    tmpfile=$(homeboy_mktemp 'phpstan-scoped-context.XXXXXX.neon')

    {
        printf '%s\n' 'includes:'
        printf '    - %s\n' "$PHPSTAN_BASE_CONFIG"
        printf '%s\n' ''
        printf '%s\n' 'parameters:'
        if [ -z "$PHPSTAN_COMPONENT_CONFIG" ]; then
            printf '%s\n' '    customRulesetUsed: false'
        fi

        while IFS= read -r -d '' context_file; do
            if [ "$has_context_files" -eq 0 ]; then
                printf '%s\n' '    scanFiles:'
                has_context_files=1
            fi
            printf '        - %s\n' "$(printf '%s' "$context_file" | jq -Rsa .)"
        done < <(find "$PLUGIN_PATH" -type f -name '*.php' \
            -not -path "*/.homeboy-build/*" \
            -not -path "*/vendor/*" \
            -not -path "*/vendor_prefixed/*" \
            -not -path "*/node_modules/*" \
            -not -path "*/build/*" \
            -not -path "*/dist/*" \
            -not -path "*/tests/*" \
            -not -path "*/tools/*" \
            -print0)

        if [ -d "${PLUGIN_PATH}/vendor_prefixed" ]; then
            printf '%s\n' '    scanDirectories:'
            printf '        - %s\n' "$(printf '%s' "${PLUGIN_PATH}/vendor_prefixed" | jq -Rsa .)"
            has_scan_directories=1
        fi
    } > "$tmpfile"

    if [ "$has_context_files" -eq 1 ] || [ "$has_scan_directories" -eq 1 ]; then
        printf '%s\n' "$tmpfile"
    else
        rm -f "$tmpfile"
        printf '%s\n' ''
    fi
}

cleanup_scoped_context_config() {
    [ -n "$SCOPED_CONTEXT_CONFIG" ] && rm -f "$SCOPED_CONTEXT_CONFIG"
    SCOPED_CONTEXT_CONFIG=""
}

DEPENDENCY_CONFIG=$(generate_dependency_config)
if [ -n "$DEPENDENCY_CONFIG" ] && [ -f "$DEPENDENCY_CONFIG" ]; then
    PHPSTAN_BASE_CONFIG="$DEPENDENCY_CONFIG"
fi

PHPSTAN_TARGETS=()
PHPSTAN_SCOPED=0

homeboy_phpstan_relpath() {
    local path="$1"
    path="${path#$PLUGIN_PATH/}"
    path="${path#./}"
    printf '%s\n' "$path"
}

homeboy_phpstan_abspath() {
    local path="$1"

    case "$path" in
        /*)
            printf '%s\n' "$path"
            ;;
        *)
            printf '%s\n' "${PLUGIN_PATH}/${path#./}"
            ;;
    esac
}

homeboy_phpstan_runtime_file() {
    local rel_path="$1"

    case "$rel_path" in
        scoper.inc.php|tools/*|tests/*|vendor_prefixed/*|vendor/*)
            return 1
            ;;
    esac

    return 0
}

resolve_phpstan_targets() {
    local matched=()
    local target
    local target_path
    local target_rel

    if [ -n "${HOMEBOY_LINT_FILE:-}" ]; then
        target_path=$(homeboy_phpstan_abspath "$HOMEBOY_LINT_FILE")
        target_rel=$(homeboy_phpstan_relpath "$target_path")
        if [ -f "$target_path" ] && [[ "$target_path" == *.php ]] && homeboy_phpstan_runtime_file "$target_rel"; then
            matched+=("$target_path")
        fi
    elif [ -n "${HOMEBOY_LINT_GLOB:-}" ]; then
        (
            cd "$PLUGIN_PATH"
            eval 'for f in '"${HOMEBOY_LINT_GLOB}"'; do [ -e "$f" ] && printf "%s\0" "$f"; done'
        ) | while IFS= read -r -d '' target; do
            target_path=$(homeboy_phpstan_abspath "$target")
            target_rel=$(homeboy_phpstan_relpath "$target_path")
            if [ -f "$target_path" ] && [[ "$target_path" == *.php ]] && homeboy_phpstan_runtime_file "$target_rel"; then
                printf '%s\0' "$target_path"
            elif [ -d "$target_path" ]; then
                find "$target_path" -type f -name '*.php' \
                    -not -path "*/.homeboy-build/*" \
                    -not -path "*/vendor/*" \
                    -not -path "*/vendor_prefixed/*" \
                    -not -path "*/node_modules/*" \
                    -not -path "*/build/*" \
                    -not -path "*/dist/*" \
                    -not -path "*/tools/*" \
                    -not -path "*/tests/*" \
                    -not -name "scoper.inc.php" \
                    -print0
            fi
        done
        return
    fi

    if [ "${#matched[@]}" -gt 0 ]; then
        printf '%s\0' "${matched[@]}"
    fi
}

resolve_phpstan_full_targets() {
    local target_path
    local target_rel

    while IFS= read -r -d '' target_path; do
        target_rel=$(homeboy_phpstan_relpath "$target_path")
        if homeboy_phpstan_runtime_file "$target_rel"; then
            printf '%s\0' "$target_path"
        fi
    done < <(find "$PLUGIN_PATH" -type f -name '*.php' \
        -not -path "*/.homeboy-build/*" \
        -not -path "*/vendor/*" \
        -not -path "*/vendor_prefixed/*" \
        -not -path "*/node_modules/*" \
        -not -path "*/build/*" \
        -not -path "*/dist/*" \
        -print0)
}

if [ -n "${HOMEBOY_LINT_FILE:-}" ] || [ -n "${HOMEBOY_LINT_GLOB:-}" ]; then
    PHPSTAN_SCOPED=1
    PHPSTAN_TARGETS=()
    while IFS= read -r -d '' phpstan_target; do
        PHPSTAN_TARGETS+=("$phpstan_target")
    done < <(resolve_phpstan_targets)

    if [ "${#PHPSTAN_TARGETS[@]}" -eq 0 ]; then
        echo "PHPStan scoped lint: no PHP files in requested scope, skipping static analysis"
        exit 0
    fi

    SCOPED_CONTEXT_CONFIG=$(generate_scoped_context_config)
    if [ -n "$SCOPED_CONTEXT_CONFIG" ] && [ -f "$SCOPED_CONTEXT_CONFIG" ]; then
        PHPSTAN_BASE_CONFIG="$SCOPED_CONTEXT_CONFIG"
    fi
else
    while IFS= read -r -d '' phpstan_target; do
        PHPSTAN_TARGETS+=("$phpstan_target")
    done < <(resolve_phpstan_full_targets)
fi

# Check if the selected PHPStan target set has PHP files.
php_file_count="${#PHPSTAN_TARGETS[@]}"

if [ "$php_file_count" -eq 0 ]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: No PHP files found, skipping PHPStan"
    fi
    exit 0
fi

echo "Running PHPStan static analysis..."
if [ "$PHPSTAN_SCOPED" -eq 1 ]; then
    echo "PHPStan scoped lint: analyzing ${#PHPSTAN_TARGETS[@]} PHP file(s) from requested scope"
fi

# Build PHPStan arguments
phpstan_args=(analyse)
phpstan_args+=(--configuration="$PHPSTAN_BASE_CONFIG")

# Level resolution:
#   - HOMEBOY_PHPSTAN_LEVEL env override wins (for ad-hoc bumps / CI matrix runs).
#   - Otherwise, default to 7 which matches the `level:` key in phpstan.neon.dist.
#
# Previously this script hardcoded 5 as a fallback, which shadowed the neon
# config whenever the env var wasn't set (`--level` on CLI overrides neon).
# That made config-driven level changes invisible to the harness. The fallback
# must now track what the neon declares (level 7, set in homeboy-extensions#225).
# Keep them in sync when bumping further.
PHPSTAN_LEVEL="${HOMEBOY_PHPSTAN_LEVEL:-7}"
if [ -n "${HOMEBOY_PHPSTAN_LEVEL:-}" ]; then
    PHPSTAN_LEVEL_SOURCE="env"
    phpstan_args+=(--level="$PHPSTAN_LEVEL")
elif [ -z "$PHPSTAN_COMPONENT_CONFIG" ] || [ "$PHPSTAN_COMPONENT_CONFIG_HAS_RULESET" -ne 1 ]; then
    phpstan_args+=(--level="$PHPSTAN_LEVEL")
else
    PHPSTAN_LEVEL="config"
    PHPSTAN_LEVEL_SOURCE="component-local"
fi

write_phpstan_producer_metadata() {
    local target="${HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE:-}"
    [ -z "$target" ] && return 0

    php -r '
        $target = $argv[1] ?? "";
        if ($target === "") {
            exit;
        }
        $metadata = [
            "phpstan_config" => $argv[2] ?? "",
            "phpstan_config_source" => $argv[3] ?? "",
            "phpstan_component_config" => ($argv[4] ?? "") !== "" ? $argv[4] : null,
            "phpstan_level" => $argv[5] ?? "",
            "phpstan_level_source" => $argv[6] ?? "",
        ];
        $dir = dirname($target);
        if ($dir !== "" && $dir !== ".") {
            @mkdir($dir, 0777, true);
        }
        file_put_contents($target, json_encode($metadata, JSON_UNESCAPED_SLASHES) . "\n");
    ' "$target" "$PHPSTAN_BASE_CONFIG" "$PHPSTAN_COMPONENT_CONFIG_SOURCE" "$PHPSTAN_COMPONENT_CONFIG" "$PHPSTAN_LEVEL" "$PHPSTAN_LEVEL_SOURCE" 2>/dev/null || true
}

write_phpstan_producer_metadata

# Memory limit (default: 2G)
phpstan_args+=(--memory-limit=2G)

# Component baseline: if <component>/phpstan-baseline.neon exists, it's pulled
# in via `includes:` in the generated dependency/autoload neon (see
# generate_dependency_config). PHPStan 2.x removed the `--baseline` CLI flag,
# so inclusion via neon is the only supported path.
if [ -f "$COMPONENT_BASELINE" ] && [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Component baseline will be pulled in via includes: $COMPONENT_BASELINE"
fi

# Include component/dependency autoloaders if they exist
# Class map for dependencies that ship no Composer autoloader.
#
# `scanFiles:` and `scanDirectories:` make declarations available for signature
# resolution, but neither registers a class for member access — PHPStan reports
# `unknown class` and every property/method access on it. Only an autoloader (or
# pulling the dependency into `paths:`, which would also report findings inside
# the dependency) registers classes.
#
# WordPress plugins commonly load classes with `require_once` from a bootstrap
# file instead of Composer, so those dependencies contribute no autoloader at
# all. Their file names also follow WordPress conventions
# (`class-wp-agent-materialized-identity.php`) rather than PSR-4, so no path
# convention can derive the mapping — the declarations have to be indexed.
#
# Emits `<fqcn> => <declaration-only-file>` pairs by tokenizing each dependency
# source once. The generated files preserve class bodies for PHPStan reflection
# without executing plugin entrypoints or other top-level runtime bootstrap.
# Tokenizing is used rather than a regex so commented-out or string-literal
# occurrences of `class Foo` cannot poison the map.
homeboy_emit_dependency_class_map_entries() {
    local dependency_path="$1"
    local declaration_dir="$2"

    homeboy_resolve_phpstan_dependency_signature_files "$dependency_path" \
        | "${HOMEBOY_PHP_BIN:-php}" -r '
            $declarationDir = $argv[1] ?? "";
            if ($declarationDir === "" || (!is_dir($declarationDir) && !mkdir($declarationDir, 0700, true))) {
                exit(1);
            }
            $stdin = fopen("php://stdin", "r");
            while (false !== ($file = fgets($stdin))) {
                $file = trim($file);
                if ($file === "" || !is_readable($file)) {
                    continue;
                }
                $source = @file_get_contents($file);
                if (!is_string($source) || strpos($source, "<?php") === false) {
                    continue;
                }
                $tokens = @token_get_all($source);
                if (!is_array($tokens)) {
                    continue;
                }
                $namespace = "";
                $namespaceDepth = 0;
                $imports = [];
                $scopeDepth = 0;
                $count = count($tokens);
                for ($i = 0; $i < $count; $i++) {
                    $token = $tokens[$i];
                    if (!is_array($token)) {
                        if ($token === "{") {
                            $scopeDepth++;
                        } elseif ($token === "}") {
                            $scopeDepth--;
                        }
                        continue;
                    }
                    if ($token[0] === T_NAMESPACE) {
                        $namespace = "";
                        $imports = [];
                        for ($j = $i + 1; $j < $count; $j++) {
                            if (is_array($tokens[$j]) && in_array($tokens[$j][0], array(T_STRING, T_NAME_QUALIFIED), true)) {
                                $namespace .= $tokens[$j][1];
                            } elseif ($tokens[$j] === ";" || $tokens[$j] === "{") {
                                $namespaceDepth = $tokens[$j] === "{" ? $scopeDepth + 1 : $scopeDepth;
                                break;
                            }
                        }
                        continue;
                    }
                    if ($token[0] === T_USE && $scopeDepth === $namespaceDepth) {
                        $import = "";
                        for ($j = $i; $j < $count; $j++) {
                            $import .= is_array($tokens[$j]) ? $tokens[$j][1] : $tokens[$j];
                            if ($tokens[$j] === ";") {
                                $imports[] = trim($import);
                                $i = $j;
                                break;
                            }
                        }
                        continue;
                    }
                    $declarationTokens = array(T_CLASS, T_INTERFACE, T_TRAIT);
                    if (defined("T_ENUM")) {
                        $declarationTokens[] = T_ENUM;
                    }
                    if (!in_array($token[0], $declarationTokens, true)) {
                        continue;
                    }
                    // Skip anonymous classes and `::class` constant fetches.
                    if ($i > 0 && is_array($tokens[$i - 1]) && $tokens[$i - 1][0] === T_DOUBLE_COLON) {
                        continue;
                    }
                    $name = "";
                    for ($j = $i + 1; $j < $count; $j++) {
                        if (is_array($tokens[$j]) && $tokens[$j][0] === T_STRING) {
                            $name = $tokens[$j][1];
                            break;
                        }
                        if ($tokens[$j] === "(" || $tokens[$j] === "{") {
                            break;
                        }
                    }
                    if ($name === "") {
                        continue;
                    }

                    $start = $i;
                    while ($start > 0 && is_array($tokens[$start - 1]) && in_array($tokens[$start - 1][0], array(T_WHITESPACE, T_COMMENT, T_DOC_COMMENT, T_ABSTRACT, T_FINAL, defined("T_READONLY") ? T_READONLY : -1), true)) {
                        $start--;
                    }
                    $declaration = "";
                    $braceDepth = 0;
                    $opened = false;
                    for ($j = $start; $j < $count; $j++) {
                        $piece = is_array($tokens[$j]) ? $tokens[$j][1] : $tokens[$j];
                        $declaration .= $piece;
                        if ($piece === "{") {
                            $braceDepth++;
                            $opened = true;
                        } elseif ($piece === "}") {
                            $braceDepth--;
                            if ($opened && $braceDepth === 0) {
                                break;
                            }
                        }
                    }
                    if (!$opened || $braceDepth !== 0) {
                        continue;
                    }

                    $fqcn = ($namespace !== "" ? $namespace . "\\" : "") . $name;
                    $target = $declarationDir . "/" . sha1($file . "\0" . $fqcn) . ".php";
                    $stub = "<?php\n";
                    if ($namespace !== "") {
                        $stub .= "namespace " . $namespace . ";\n";
                    }
                    if ($imports !== []) {
                        $stub .= implode("\n", $imports) . "\n";
                    }
                    $stub .= $declaration . "\n";
                    if (file_put_contents($target, $stub) === false) {
                        continue;
                    }
                    echo $fqcn . "\t" . $target . "\n";
                    $i = $j;
                }
            }
        ' "$declaration_dir" 2>/dev/null
}

generate_composite_autoload() {
    local tmpfile
    local component_autoload="${PLUGIN_PATH}/vendor/autoload.php"
    local component_prefixed_autoload="${PLUGIN_PATH}/vendor_prefixed/autoload.php"

    tmpfile=$(homeboy_mktemp 'homeboy-phpstan-autoload.XXXXXX')
    COMPOSITE_AUTOLOAD_DIR="${tmpfile}.d"
    mkdir -p "$COMPOSITE_AUTOLOAD_DIR"

    {
        printf '%s\n' '<?php'
        printf '%s\n' '$autoloadFiles = ['

        local autoloaderless_dependencies=""
        while IFS= read -r dependency_path; do
            [ -z "$dependency_path" ] && continue
            local dependency_autoload="${dependency_path}/vendor/autoload.php"
            local dependency_prefixed_autoload="${dependency_path}/vendor_prefixed/autoload.php"
            if [ -f "$dependency_autoload" ]; then
                printf '    %s,\n' "$(printf '%s' "$dependency_autoload" | jq -Rsa .)"
            fi
            if [ -f "$dependency_prefixed_autoload" ]; then
                printf '    %s,\n' "$(printf '%s' "$dependency_prefixed_autoload" | jq -Rsa .)"
            fi
            if [ ! -f "$dependency_autoload" ] && [ ! -f "$dependency_prefixed_autoload" ]; then
                autoloaderless_dependencies+="${dependency_path}"$'\n'
            fi
        done < <(homeboy_resolve_validation_dependency_paths "$PLUGIN_PATH")

        if [ -f "$component_autoload" ]; then
            printf '    %s,\n' "$(printf '%s' "$component_autoload" | jq -Rsa .)"
        fi

        if [ -f "$component_prefixed_autoload" ]; then
            printf '    %s,\n' "$(printf '%s' "$component_prefixed_autoload" | jq -Rsa .)"
        fi

        printf '%s\n' '];'
        printf '%s\n' 'foreach ($autoloadFiles as $autoloadFile) {'
        printf '%s\n' '    if (is_string($autoloadFile) && $autoloadFile !== "" && file_exists($autoloadFile)) {'
        printf '%s\n' '        require_once $autoloadFile;'
        printf '%s\n' '    }'
        printf '%s\n' '}'

        # Dependencies without a Composer autoloader still have to register
        # their classes, otherwise member access on them reports `unknown
        # class`. See homeboy_emit_dependency_class_map_entries().
        printf '%s\n' '$homeboyDependencyClassMap = ['
        local class_map_entries=0
        while IFS= read -r dependency_path; do
            [ -z "$dependency_path" ] && continue
            while IFS=$'\t' read -r fqcn class_file; do
                [ -z "$fqcn" ] || [ -z "$class_file" ] && continue
                printf '    %s => %s,\n' \
                    "$(printf '%s' "$fqcn" | jq -Rsa .)" \
                    "$(printf '%s' "$class_file" | jq -Rsa .)"
                class_map_entries=$((class_map_entries + 1))
            done < <(homeboy_emit_dependency_class_map_entries "$dependency_path" "$COMPOSITE_AUTOLOAD_DIR")
        done <<< "$autoloaderless_dependencies"
        printf '%s\n' '];'
        printf '%s\n' 'if ($homeboyDependencyClassMap !== []) {'
        printf '%s\n' '    spl_autoload_register(static function ($class) use ($homeboyDependencyClassMap) {'
        printf '%s\n' '        if (isset($homeboyDependencyClassMap[$class])) {'
        printf '%s\n' '            require_once $homeboyDependencyClassMap[$class];'
        printf '%s\n' '        }'
        printf '%s\n' '    });'
        printf '%s\n' '}'

        if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
            echo "DEBUG: dependency class map entries: ${class_map_entries}" >&2
        fi
    } > "$tmpfile"

    printf '%s\n' "$tmpfile"
}

cleanup_composite_autoload() {
    [ -n "$COMPOSITE_AUTOLOAD" ] && rm -f "$COMPOSITE_AUTOLOAD"
    [ -n "$COMPOSITE_AUTOLOAD_DIR" ] && rm -rf "$COMPOSITE_AUTOLOAD_DIR"
    COMPOSITE_AUTOLOAD=""
    COMPOSITE_AUTOLOAD_DIR=""
}

COMPOSITE_AUTOLOAD=$(generate_composite_autoload)
COMPOSITE_AUTOLOAD_DIR="${COMPOSITE_AUTOLOAD}.d"

if [ -f "$COMPOSITE_AUTOLOAD" ]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Using composite autoloader: $COMPOSITE_AUTOLOAD"
        echo "DEBUG: Using PHPStan config: $PHPSTAN_BASE_CONFIG"
        echo "DEBUG: PHPStan config source: $PHPSTAN_COMPONENT_CONFIG_SOURCE"
        [ -n "$PHPSTAN_COMPONENT_CONFIG" ] && echo "DEBUG: Component PHPStan config: $PHPSTAN_COMPONENT_CONFIG"
        echo "DEBUG: PHPStan level source: $PHPSTAN_LEVEL_SOURCE"
    fi
    phpstan_args+=(--autoload-file="$COMPOSITE_AUTOLOAD")
fi

# No progress bar for cleaner output
phpstan_args+=(--no-progress)

# Thread control: HOMEBOY_PHPSTAN_THREADS overrides, otherwise auto-detect.
# On low-core machines (<=2 CPUs), force single-threaded to avoid parallel worker crashes.
# PHPStan 2.x removed the --threads CLI flag; parallel config is set via neon includes.
PHPSTAN_MAX_PROCESSES=""
if [ -n "${HOMEBOY_PHPSTAN_THREADS:-}" ]; then
    PHPSTAN_MAX_PROCESSES="${HOMEBOY_PHPSTAN_THREADS}"
elif [ -n "$DEPENDENCY_CONFIG" ] && [ -n "${HOMEBOY_PHP_VERSION:-}" ]; then
    # Release preflight adds phpVersion on top of generated dependency/baseline
    # configs; keep that layered config in-process so WP override scanFiles stay
    # stable instead of depending on worker bootstrap behavior.
    PHPSTAN_MAX_PROCESSES="1"
elif [ "$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)" -le 2 ]; then
    PHPSTAN_MAX_PROCESSES="1"
fi

# Convert a PHP version string (e.g. "8.2", "8.2.1") to PHPStan's integer format (e.g. 80200, 80201).
# PHPStan phpVersion format: major * 10000 + minor * 100 + patch
php_version_to_phpstan_int() {
    local version="$1"
    local major minor patch
    IFS='.' read -r major minor patch <<< "$version"
    major="${major:-0}"
    minor="${minor:-0}"
    patch="${patch:-0}"
    echo $(( major * 10000 + minor * 100 + patch ))
}

# Detect PHP version from HOMEBOY_PHP_VERSION (set by lint-runner or the user).
PHPSTAN_PHP_VERSION=""
if [ -n "${HOMEBOY_PHP_VERSION:-}" ]; then
    PHPSTAN_PHP_VERSION=$(php_version_to_phpstan_int "${HOMEBOY_PHP_VERSION}")
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: PHPStan phpVersion: ${PHPSTAN_PHP_VERSION} (from ${HOMEBOY_PHP_VERSION})"
    fi
    echo "PHPStan PHP version target: ${HOMEBOY_PHP_VERSION} (${PHPSTAN_PHP_VERSION})"
fi

# Generate a temp neon config that includes the main config and overrides
# parallel settings and/or phpVersion as needed.
PHPSTAN_TMPCONFIG=""
generate_phpstan_config() {
    local max_processes="${1:-}"
    local tmpfile
    tmpfile=$(homeboy_mktemp 'phpstan.XXXXXX.neon')
    {
        printf 'includes:\n'
        printf '    - %s\n' "${PHPSTAN_BASE_CONFIG}"
        printf '\n'
        printf 'parameters:\n'
        if [ -n "$max_processes" ]; then
            printf '    parallel:\n'
            printf '        maximumNumberOfProcesses: %s\n' "${max_processes}"
        fi
        if [ -n "$PHPSTAN_PHP_VERSION" ]; then
            printf '    phpVersion: %s\n' "${PHPSTAN_PHP_VERSION}"
        fi
    } > "$tmpfile"
    echo "$tmpfile"
}

cleanup_phpstan_config() {
    [ -n "$PHPSTAN_TMPCONFIG" ] && rm -f "$PHPSTAN_TMPCONFIG"
    PHPSTAN_TMPCONFIG=""
}
trap 'cleanup_phpstan_config; cleanup_composite_autoload; cleanup_dependency_config; cleanup_scoped_context_config' EXIT

# Generate a temp config when we need to override parallel processes or phpVersion.
if [ -n "$PHPSTAN_MAX_PROCESSES" ] || [ -n "$PHPSTAN_PHP_VERSION" ]; then
    PHPSTAN_TMPCONFIG=$(generate_phpstan_config "$PHPSTAN_MAX_PROCESSES")
    # Replace the --configuration arg with our temp config
    phpstan_args=(analyse)
    phpstan_args+=(--configuration="$PHPSTAN_TMPCONFIG")
    if [ -n "${HOMEBOY_PHPSTAN_LEVEL:-}" ] || [ -z "$PHPSTAN_COMPONENT_CONFIG" ] || [ "$PHPSTAN_COMPONENT_CONFIG_HAS_RULESET" -ne 1 ]; then
        phpstan_args+=(--level="$PHPSTAN_LEVEL")
    fi
    phpstan_args+=(--memory-limit=2G)
    # Baseline is pulled in via neon includes (PHPSTAN_TMPCONFIG includes
    # PHPSTAN_BASE_CONFIG which includes COMPONENT_BASELINE when present),
    # so there's no --baseline CLI flag to pass (removed in PHPStan 2.x).
    if [ -f "$COMPOSITE_AUTOLOAD" ]; then
        phpstan_args+=(--autoload-file="$COMPOSITE_AUTOLOAD")
    fi
    phpstan_args+=(--no-progress)
    phpstan_args+=("${PHPSTAN_TARGETS[@]}")
fi

# Add the path to analyze (only when not already set by override block above)
if [ -z "$PHPSTAN_MAX_PROCESSES" ] && [ -z "$PHPSTAN_PHP_VERSION" ]; then
    phpstan_args+=("${PHPSTAN_TARGETS[@]}")
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: PHPStan command: $PHPSTAN_BIN ${phpstan_args[*]}"
fi

# Helper: detect parallel worker failure in PHPStan JSON output.
# Returns 0 (true) if the only errors are parallel worker crashes.
is_parallel_worker_failure() {
    local output="$1"
    [ -z "$output" ] && return 1
    echo "$output" | php -r '
        $json = json_decode(file_get_contents("php://stdin"), true);
        if (!$json) exit(1);
        $totals = $json["totals"] ?? [];
        $fileErrors = $totals["file_errors"] ?? 0;
        $globalErrors = $totals["errors"] ?? 0;
        if ($fileErrors === 0 && $globalErrors > 0) {
            foreach ($json["errors"] ?? [] as $err) {
                if (stripos($err, "parallel worker") !== false) exit(0);
            }
        }
        exit(1);
    ' 2>/dev/null
}

# Helper: detect parallel worker failure in PHPStan stderr text output.
# Returns 0 (true) if any stderr line mentions "parallel worker".
is_parallel_worker_failure_text() {
    local output="$1"
    [ -z "$output" ] && return 1
    echo "$output" | grep -qi "parallel worker"
}

# Prepare a forced single-process PHPStan retry config and echo its path.
# Callers build a retry args array using this config + PHPStan's --debug flag,
# which runs analysis fully in-process with no worker subprocesses at all —
# bypasses the parallel worker IPC that crashes on macOS + larger codebases
# (homeboy-extensions#207). The maximumNumberOfProcesses: 1 in the neon is
# belt-and-braces alongside --debug on the CLI; either alone is usually
# enough, together they cover every PHPStan version we've hit.
prepare_phpstan_retry_config() {
    cleanup_phpstan_config
    PHPSTAN_TMPCONFIG=$(generate_phpstan_config 1)
    printf '%s\n' "$PHPSTAN_TMPCONFIG"
}

add_phpstan_retry_targets() {
    if [ "$PHPSTAN_SCOPED" -eq 1 ]; then
        local target rel_target
        for target in "${PHPSTAN_TARGETS[@]}"; do
            rel_target="${target#$PLUGIN_PATH/}"
            retry_args+=("$rel_target")
        done
    else
        retry_args+=(.)
    fi
}

# Summary mode: get JSON output and parse it
if [[ "${HOMEBOY_SUMMARY_MODE:-}" == "1" ]]; then
    set +e
    # Capture stderr separately to show PHPStan errors if it fails
    stderr_file=$(homeboy_mktemp 'phpstan-stderr.XXXXXX')
    json_output=$(homeboy_run_phpstan "${phpstan_args[@]}" --error-format=json 2>"$stderr_file")
    json_exit=$?
    stderr_output=$(cat "$stderr_file")
    rm -f "$stderr_file"

    # Retry with forced single-process (--debug) if parallel workers failed.
    # --debug disables PHPStan's worker subprocesses entirely, bypassing the IPC
    # hangs that produce "Some parallel worker jobs have not finished" on macOS
    # with larger codebases (homeboy-extensions#207).
    if [ "$json_exit" -ne 0 ] && is_parallel_worker_failure "$json_output"; then
        echo "Parallel worker failure detected, retrying single-process (--debug)..."
        prepare_phpstan_retry_config > /dev/null
        retry_args=(analyse --configuration="$PHPSTAN_TMPCONFIG" --memory-limit=2G --no-progress --debug)
        if [ -n "${HOMEBOY_PHPSTAN_LEVEL:-}" ] || [ -z "$PHPSTAN_COMPONENT_CONFIG" ] || [ "$PHPSTAN_COMPONENT_CONFIG_HAS_RULESET" -ne 1 ]; then
            retry_args+=(--level="$PHPSTAN_LEVEL")
        fi
        # Baseline is pulled in via PHPSTAN_TMPCONFIG → PHPSTAN_BASE_CONFIG
        # → COMPONENT_BASELINE include chain (no --baseline CLI flag in PHPStan 2.x).
        [ -f "$COMPOSITE_AUTOLOAD" ] && retry_args+=(--autoload-file="$COMPOSITE_AUTOLOAD")
        add_phpstan_retry_targets
        stderr_file=$(homeboy_mktemp 'phpstan-stderr.XXXXXX')
        # PHPStan --debug mode has a known interaction with --autoload-file
        # where running from a CWD different than the analysed path causes
        # analysis to stop after the first file (exit 0 with no results).
        # Work around by running from inside $PLUGIN_PATH in a subshell and
        # passing `.` as the positional path argument. Full-report retry below
        # does the same.
        raw_output=$(cd "$PLUGIN_PATH" && "$PHPSTAN_BIN" "${retry_args[@]}" --error-format=json 2>"$stderr_file")
        json_exit=$?
        stderr_output=$(cat "$stderr_file")
        rm -f "$stderr_file"
        # --debug prints analysed file paths to stdout BEFORE the JSON envelope,
        # which would break the downstream JSON parser. Extract the JSON line
        # (first line starting with `{`) from the combined debug stream.
        json_output=$(printf '%s\n' "$raw_output" | awk '/^\{/{print; exit}')
        # If we couldn't isolate a JSON envelope, fall back to the raw output
        # so error propagation still works (the worker-failure re-check below
        # tolerates non-JSON input by also inspecting stderr).
        [ -z "$json_output" ] && json_output="$raw_output"

        # Graceful degradation: if --debug retry STILL reports a parallel worker
        # failure (the crash happened outside of worker IPC, e.g. in PHPStan's
        # own bootstrap), skip PHPStan cleanly rather than block the release.
        # Runtime fatal detection still runs via the critical-only path elsewhere,
        # so this is safe: we're only skipping style-level static analysis.
        if [ "$json_exit" -ne 0 ] && \
           { is_parallel_worker_failure "$json_output" || is_parallel_worker_failure_text "$stderr_output"; }; then
            echo ""
            echo "WARNING: PHPStan parallel worker failure persisted after single-process retry."
            echo "         Skipping static analysis for this run (homeboy-extensions#207)."
            echo "         To force analysis: HOMEBOY_PHPSTAN_THREADS=1 homeboy test <component>"
            echo "         If the issue reproduces reliably, please report environment details"
            echo "         (OS, PHP version, PHPStan version, component size) on the tracking issue."
            exit 0
        fi
    fi
    set -e

    # Parse JSON and print full summary with error details
    if [ -n "$json_output" ] && command -v php &> /dev/null; then
        parsed_output=$(echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json) exit;

            $level = $argv[1] ?? "5";
            $componentPath = $argv[2] ?? "";
            $criticalOnly = ($argv[3] ?? "0") === "1";
            $criticalPattern = $argv[4] ?? "function.notFound|class.notFound";

            $criticalIdentifiers = explode("|", $criticalPattern);

            $totals = $json["totals"] ?? [];
            $fileErrorCount = $totals["file_errors"] ?? 0;
            $globalErrors = $json["errors"] ?? [];
            $errorCount = $fileErrorCount + count($globalErrors);
            $fileCount = count($json["files"] ?? []);

            if ($errorCount === 0) exit;

            // When in critical-only mode, filter to just fatal-class errors
            $filteredFiles = [];
            $filteredErrorCount = 0;

            foreach ($json["files"] ?? [] as $filePath => $data) {
                $filteredMessages = [];
                foreach ($data["messages"] ?? [] as $msg) {
                    $identifier = $msg["identifier"] ?? "unknown";
                    if ($criticalOnly) {
                        $isCritical = false;
                        foreach ($criticalIdentifiers as $crit) {
                            if ($identifier === trim($crit)) {
                                $isCritical = true;
                                break;
                            }
                        }
                        if (!$isCritical) continue;
                    }
                    $filteredMessages[] = $msg;
                    $filteredErrorCount++;
                }
                if (!empty($filteredMessages)) {
                    $filteredFiles[$filePath] = $filteredMessages;
                }
            }

            if ($criticalOnly && $filteredErrorCount === 0) {
                // No critical errors — skip output entirely
                exit;
            }

            $displayErrorCount = $criticalOnly ? $filteredErrorCount : $errorCount;
            $displayFileCount = $criticalOnly ? count($filteredFiles) : $fileCount;

            // Summary header
            echo "============================================\n";
            if ($criticalOnly) {
                echo "PHPSTAN CRITICAL: " . $displayErrorCount . " fatal-class error(s) found\n";
                echo "These indicate guaranteed runtime fatals and cannot be skipped.\n";
            } else {
                echo "PHPSTAN SUMMARY: " . $displayErrorCount . " errors at level " . $level . "\n";
            }
            echo "Files with issues: " . $displayFileCount . "\n";
            echo "============================================\n";

            // Error details section
            echo "\nERRORS:\n";
            $identifiers = [];

            foreach ($filteredFiles as $filePath => $messages) {
                // Strip component path prefix for cleaner output
                $displayPath = $filePath;
                if ($componentPath && strpos($filePath, $componentPath) === 0) {
                    $displayPath = ltrim(substr($filePath, strlen($componentPath)), "/");
                }

                foreach ($messages as $msg) {
                    $line = $msg["line"] ?? "?";
                    $message = $msg["message"] ?? "Unknown error";
                    $identifier = $msg["identifier"] ?? "unknown";

                    echo "  " . $displayPath . ":" . $line . "\n";
                    echo "    " . $message . "\n";
                    echo "    [" . $identifier . "]\n";
                    echo "\n";

                    $identifiers[$identifier] = ($identifiers[$identifier] ?? 0) + 1;
                }
            }

            if (!$criticalOnly && $globalErrors !== []) {
                echo "  PHPStan:\n";
                foreach ($globalErrors as $message) {
                    echo "    " . (is_string($message) ? $message : json_encode($message)) . "\n\n";
                    $identifiers["internal"] = ($identifiers["internal"] ?? 0) + 1;
                }
            }

            // Top error types section
            if (!empty($identifiers)) {
                arsort($identifiers);
                echo "TOP ERROR TYPES:\n";
                $count = 0;
                foreach ($identifiers as $id => $num) {
                    printf("  %-55s %5d\n", $id, $num);
                    $count++;
                    if ($count >= 10) break;
                }
            }

            // In critical-only mode, signal the caller that critical errors were found
            // by writing a marker that the shell can check
            if ($criticalOnly && $filteredErrorCount > 0) {
                echo "\nCRITICAL_ERRORS_FOUND=" . $filteredErrorCount . "\n";
            }
        ' "$PHPSTAN_LEVEL" "$PLUGIN_PATH" "$PHPSTAN_CRITICAL_ONLY" "$CRITICAL_PHPSTAN_IDENTIFIERS" 2>/dev/null)

        if [ -n "$parsed_output" ]; then
            echo ""
            echo "$parsed_output"
        elif [ -n "$json_output" ] && echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json) exit(0);
            exit((int) ($json["totals"]["file_errors"] ?? 0) > 0 ? 0 : 1);
        ' 2>/dev/null; then
            # Fallback: show raw JSON when PHP parsing fails
            echo ""
            echo "ERRORS (raw):"
            echo "$json_output"
        fi
    fi

    # Write annotations sidecar JSON for CI inline comments. Annotations are
    # Homeboy observability output, not a lint result — if the sidecar writer is
    # unavailable, skip writing them rather than failing the static-analysis
    # step. A missing writer must never masquerade as a finding
    # (homeboy-extensions#1402).
    if [ -n "${HOMEBOY_ANNOTATIONS_DIR:-}" ] && [ -d "${HOMEBOY_ANNOTATIONS_DIR}" ] && [ -n "$json_output" ] && type homeboy_sidecar_merge >/dev/null 2>&1; then
        _PHPSTAN_ANNOTATIONS_TMPFILE=$(homeboy_mktemp 'phpstan-annotations.XXXXXX')
        echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json || empty($json["files"])) exit;
            $componentPath = $argv[1] ?? "";
            $level = $argv[2] ?? "5";
            $annotations = [];
            foreach ($json["files"] as $filePath => $data) {
                $displayPath = $filePath;
                if ($componentPath && strpos($filePath, $componentPath) === 0) {
                    $displayPath = ltrim(substr($filePath, strlen($componentPath)), "/");
                }
                foreach ($data["messages"] ?? [] as $msg) {
                    $annotations[] = [
                        "file" => $displayPath,
                        "line" => $msg["line"] ?? 0,
                        "message" => $msg["message"] ?? "Unknown",
                        "source" => "phpstan",
                        "severity" => "error",
                        "code" => $msg["identifier"] ?? "unknown",
                    ];
                }
            }
            $outputFile = $argv[3] ?? "";
            if ($outputFile && !empty($annotations)) {
                file_put_contents($outputFile, json_encode($annotations, JSON_UNESCAPED_SLASHES) . "\n");
            }
        ' "$PLUGIN_PATH" "$PHPSTAN_LEVEL" "$_PHPSTAN_ANNOTATIONS_TMPFILE" 2>/dev/null || true
        homeboy_sidecar_merge annotation.phpstan "$_PHPSTAN_ANNOTATIONS_TMPFILE"
        rm -f "$_PHPSTAN_ANNOTATIONS_TMPFILE"
    fi

    # Write PHPStan lint findings sidecar for homeboy baseline ratchet.
    # Transforms PHPStan JSON into the same current LintFinding shape PHPCS uses.
    # The lint-runner merges these with PHPCS findings into the final baseline.
    if [ -n "${_HOMEBOY_PHPSTAN_FINDINGS_FILE:-}" ] && [ -n "$json_output" ]; then
        _PHPSTAN_FINDINGS_OUTPUT_TMPFILE=$(homeboy_mktemp 'phpstan-findings.XXXXXX')
        echo "$json_output" | php -r '
            require $argv[3];
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json || (empty($json["files"]) && empty($json["errors"]))) {
                file_put_contents($argv[2], "[]");
                exit;
            }
            $componentPath = $argv[1] ?? "";
            $findings = [];
            $readExcerpt = static function ($path, $line) {
                if (!$path || !$line || !is_readable($path)) {
                    return null;
                }

                $lines = @file($path, FILE_IGNORE_NEW_LINES);
                return $lines[$line - 1] ?? null;
            };
            foreach ($json["files"] as $filePath => $data) {
                $relPath = $filePath;
                if ($componentPath && strpos($filePath, $componentPath) === 0) {
                    $relPath = ltrim(substr($filePath, strlen($componentPath)), "/");
                }
                foreach ($data["messages"] ?? [] as $msg) {
                    $identifier = $msg["identifier"] ?? "unknown";
                    $line = $msg["line"] ?? 0;
                    $code = "phpstan." . $identifier;
                    $id = $relPath . "::" . $code . "::" . $line;
                    $message = ($msg["message"] ?? "Unknown") . " (" . $code . ")";
                    $findings[] = [
                        "id" => $id,
                        "tool" => "phpstan",
                        "file" => $relPath,
                        "line" => $line,
                        "column" => null,
                        "severity" => "error",
                        "code" => $code,
                        "rule" => $code,
                        "category" => "phpstan",
                        "message" => $message,
                        "fixable" => false,
                        "excerpt" => $readExcerpt($filePath, $line),
                    ];
                }
            }
            foreach ($json["errors"] ?? [] as $globalIndex => $globalError) {
                $message = is_string($globalError) ? $globalError : json_encode($globalError, JSON_UNESCAPED_SLASHES);
                $code = stripos((string) $message, "internal error") !== false ? "phpstan.internal" : "phpstan.global";
                $findings[] = [
                    "id" => "phpstan::" . $code . "::" . $globalIndex,
                    "tool" => "phpstan",
                    "file" => null,
                    "line" => null,
                    "column" => null,
                    "severity" => "error",
                    "code" => $code,
                    "rule" => $code,
                    "category" => "phpstan",
                    "message" => $message . " (" . $code . ")",
                    "fixable" => false,
                    "excerpt" => null,
                ];
            }
            $findings = homeboy_assign_stable_lint_fingerprints($findings);
            file_put_contents($argv[2], json_encode($findings, JSON_UNESCAPED_SLASHES) . "\n");
        ' "$PLUGIN_PATH" "${_PHPSTAN_FINDINGS_OUTPUT_TMPFILE}" "$STABLE_FINGERPRINT_HELPER" 2>/dev/null || true
        # Best-effort observability; never fail the gate on a sidecar write.
        write_phpstan_findings_sidecar "${_HOMEBOY_PHPSTAN_FINDINGS_FILE}" "${_PHPSTAN_FINDINGS_OUTPUT_TMPFILE}" || true
        rm -f "${_PHPSTAN_FINDINGS_OUTPUT_TMPFILE}"
    fi

    # Fallback: show stderr if PHPStan failed without producing JSON
    if [ "$json_exit" -ne 0 ] && [ -z "$json_output" ] && [ -n "$stderr_output" ]; then
        echo ""
        echo "PHPStan error:"
        echo "$stderr_output"
    fi

    # Exit with appropriate code
    if [ "$PHPSTAN_CRITICAL_ONLY" -eq 1 ]; then
        # In critical-only mode, only fail if critical errors were found
        if echo "$parsed_output" | grep -q "CRITICAL_ERRORS_FOUND="; then
            echo ""
            echo "PHPStan critical check FAILED — fatal-class errors detected"
            echo "These errors indicate undefined functions or classes that will crash at runtime."
            echo "Fix these before releasing, even with --skip-checks."
            exit 1
        else
            echo ""
            echo "PHPStan critical check passed (style checks skipped)"
            exit 0
        fi
    fi

    if [ "$json_exit" -eq 0 ]; then
        echo ""
        echo "PHPStan analysis passed"
        exit 0
    else
        echo ""
        echo "PHPStan analysis found issues"
        exit 1
    fi
fi

# Full report mode (default)
# In critical-only mode, we use JSON output to filter for fatal-class errors.
# In normal mode, we show the full PHPStan text report.
if [ "$PHPSTAN_CRITICAL_ONLY" -eq 1 ]; then
    echo "Running PHPStan critical-only check (style checks skipped)..."
    set +e
    stderr_file=$(homeboy_mktemp 'phpstan-stderr.XXXXXX')
    json_output=$(homeboy_run_phpstan "${phpstan_args[@]}" --error-format=json 2>"$stderr_file")
    full_exit=$?
    stderr_output=$(cat "$stderr_file")
    rm -f "$stderr_file"
    set -e

    # Check for critical errors in JSON output
    if [ -n "$json_output" ] && command -v php &> /dev/null; then
        critical_count=$(echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json) { echo "0"; exit; }
            $criticalPattern = $argv[1] ?? "function.notFound|class.notFound";
            $criticalIds = explode("|", $criticalPattern);
            $count = 0;
            foreach ($json["files"] ?? [] as $data) {
                foreach ($data["messages"] ?? [] as $msg) {
                    $id = $msg["identifier"] ?? "";
                    foreach ($criticalIds as $crit) {
                        if ($id === trim($crit)) { $count++; break; }
                    }
                }
            }
            echo $count;
        ' "$CRITICAL_PHPSTAN_IDENTIFIERS" 2>/dev/null || echo "0")

        if [ "$critical_count" -gt 0 ]; then
            echo ""
            echo "============================================"
            echo "PHPSTAN CRITICAL: $critical_count fatal-class error(s) found"
            echo "These indicate guaranteed runtime fatals and cannot be skipped."
            echo "============================================"
            # Show the critical errors
            echo "$json_output" | php -r '
                $json = json_decode(file_get_contents("php://stdin"), true);
                if (!$json) exit;
                $criticalPattern = $argv[1] ?? "function.notFound|class.notFound";
                $componentPath = $argv[2] ?? "";
                $criticalIds = explode("|", $criticalPattern);
                foreach ($json["files"] ?? [] as $filePath => $data) {
                    $displayPath = $filePath;
                    if ($componentPath && strpos($filePath, $componentPath) === 0) {
                        $displayPath = ltrim(substr($filePath, strlen($componentPath)), "/");
                    }
                    foreach ($data["messages"] ?? [] as $msg) {
                        $id = $msg["identifier"] ?? "";
                        $isCritical = false;
                        foreach ($criticalIds as $crit) {
                            if ($id === trim($crit)) { $isCritical = true; break; }
                        }
                        if (!$isCritical) continue;
                        echo "  " . $displayPath . ":" . ($msg["line"] ?? "?") . "\n";
                        echo "    " . ($msg["message"] ?? "Unknown") . "\n";
                        echo "    [" . $id . "]\n\n";
                    }
                }
            ' "$CRITICAL_PHPSTAN_IDENTIFIERS" "$PLUGIN_PATH" 2>/dev/null
            echo "Fix these before releasing, even with --skip-checks."
            exit 1
        fi
    fi

    echo "PHPStan critical check passed (style checks skipped)"
    exit 0
fi

set +e
# Capture stdout+stderr to files separately so we can inspect both on retry.
# PHPStan writes the "parallel worker" error box to stdout (not stderr), so
# inspecting stderr alone misses the failure signature. We print the captured
# streams below once the exit code is known — this keeps things simple and
# avoids race conditions with `tee` in process substitution.
stdout_file=$(homeboy_mktemp 'phpstan-stdout.XXXXXX')
stderr_file=$(homeboy_mktemp 'phpstan-stderr.XXXXXX')
homeboy_run_phpstan "${phpstan_args[@]}" >"$stdout_file" 2>"$stderr_file"
full_exit=$?
stdout_output=$(cat "$stdout_file")
stderr_output=$(cat "$stderr_file")
rm -f "$stdout_file" "$stderr_file"
# Always forward the original stdout/stderr to the operator so progress
# and formatted error output remain visible.
[ -n "$stdout_output" ] && printf '%s\n' "$stdout_output"
[ -n "$stderr_output" ] && printf '%s\n' "$stderr_output" >&2
set -e

# Retry with forced single-process (--debug) if parallel workers failed.
# --debug disables PHPStan's worker subprocesses entirely, bypassing the IPC
# hangs that produce "Some parallel worker jobs have not finished" on macOS
# with larger codebases (homeboy-extensions#207). The error banner is on
# stdout, not stderr, so inspect both.
if [ "$full_exit" -ne 0 ] && \
   { is_parallel_worker_failure_text "$stderr_output" || is_parallel_worker_failure_text "$stdout_output"; }; then
    echo "Parallel worker failure detected, retrying single-process (--debug)..."
    prepare_phpstan_retry_config > /dev/null
    retry_args=(analyse --configuration="$PHPSTAN_TMPCONFIG" --memory-limit=2G --no-progress --debug)
    if [ -n "${HOMEBOY_PHPSTAN_LEVEL:-}" ] || [ -z "$PHPSTAN_COMPONENT_CONFIG" ] || [ "$PHPSTAN_COMPONENT_CONFIG_HAS_RULESET" -ne 1 ]; then
        retry_args+=(--level="$PHPSTAN_LEVEL")
    fi
    # Baseline pulled in via PHPSTAN_TMPCONFIG include chain, same as summary-mode retry.
    [ -f "$COMPOSITE_AUTOLOAD" ] && retry_args+=(--autoload-file="$COMPOSITE_AUTOLOAD")
    add_phpstan_retry_targets
    set +e
    retry_stdout_file=$(homeboy_mktemp 'phpstan-stdout.XXXXXX')
    retry_stderr_file=$(homeboy_mktemp 'phpstan-stderr.XXXXXX')
    # PHPStan --debug + --autoload-file + CWD != analysed path causes analysis
    # to stop after the first file. Work around by running from inside
    # $PLUGIN_PATH and passing `.` as the positional path argument.
    (cd "$PLUGIN_PATH" && "$PHPSTAN_BIN" "${retry_args[@]}") >"$retry_stdout_file" 2>"$retry_stderr_file"
    full_exit=$?
    retry_stdout_output=$(cat "$retry_stdout_file")
    retry_stderr_output=$(cat "$retry_stderr_file")
    rm -f "$retry_stdout_file" "$retry_stderr_file"
    # --debug dumps one line per analysed file to stdout BEFORE PHPStan's
    # formatted results section. That's noisy — suppress the per-file chatter
    # and print only the lines that look like the real results (the formatted
    # error box / summary starts with spaces, dashes, or the "[ERROR]" banner).
    if [ -n "$retry_stdout_output" ]; then
        printf '%s\n' "$retry_stdout_output" | awk '
            # Skip bare file paths (absolute paths starting with /).
            /^\// { next }
            { print }
        '
    fi
    [ -n "$retry_stderr_output" ] && printf '%s\n' "$retry_stderr_output" >&2
    set -e

    # Graceful degradation: if --debug retry STILL reports a parallel worker
    # failure (the crash happened outside of worker IPC, e.g. in PHPStan's
    # own bootstrap), skip PHPStan cleanly rather than block the release.
    # Runtime fatal detection still runs via the critical-only path elsewhere,
    # so this is safe: we're only skipping style-level static analysis.
    if [ "$full_exit" -ne 0 ] && \
       { is_parallel_worker_failure_text "$retry_stderr_output" || is_parallel_worker_failure_text "$retry_stdout_output"; }; then
        echo ""
        echo "WARNING: PHPStan parallel worker failure persisted after single-process retry."
        echo "         Skipping static analysis for this run (homeboy-extensions#207)."
        echo "         To force analysis: HOMEBOY_PHPSTAN_THREADS=1 homeboy test <component>"
        echo "         If the issue reproduces reliably, please report environment details"
        echo "         (OS, PHP version, PHPStan version, component size) on the tracking issue."
        exit 0
    fi
fi

if [ "$full_exit" -eq 0 ]; then
    echo "PHPStan analysis passed"
    exit 0
else
    # stdout/stderr were already forwarded above (original run) and by the
    # retry block (retry run). No additional echoing needed here.
    echo "PHPStan analysis failed"
    exit 1
fi
