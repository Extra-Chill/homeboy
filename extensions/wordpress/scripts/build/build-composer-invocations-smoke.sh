#!/usr/bin/env bash
# The build runs composer only as often as the source checkout requires.
#
# A persistent checkout gets the production install plus exactly one dev
# restore, including when a staged file fails PHP lint after production deps
# replaced vendor/. A disposable source (HOMEBOY_BUILD_SOURCE_DISPOSABLE=1)
# gets only the production install. A stub composer on PATH records every
# invocation.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-composer-invocations.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

fake_bin="${TMP_DIR}/bin"
mkdir -p "$fake_bin"
cat > "${fake_bin}/composer" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$COMPOSER_INVOCATION_LOG"
if [ "${1:-}" = "install" ]; then
    mkdir -p vendor
    printf '<?php\n' > vendor/autoload.php
fi
SH
chmod +x "${fake_bin}/composer"

PROD_INSTALL="install --no-dev --optimize-autoloader --prefer-dist --no-interaction --quiet"
DEV_RESTORE="install --no-interaction --quiet"
VALID_PHP='<?php function composer_smoke_thing() { return true; }'
BROKEN_PHP='<?php function composer_smoke_thing( { return true; }'

fail=0
case_index=0

# check_case <label> <disposable 0|1> <php body> <expected exit: ok|fail> <expected invocation>...
check_case() {
    local label="$1" disposable="$2" body="$3" expected_exit="$4"
    shift 4
    case_index=$((case_index + 1))

    local dir="${TMP_DIR}/case-${case_index}/composer-smoke"
    local log="${TMP_DIR}/case-${case_index}/composer.log"
    mkdir -p "${dir}/inc"
    cat > "${dir}/composer-smoke.php" <<'PHP'
<?php
/**
 * Plugin Name: Composer Smoke
 * Version: 1.0.0
 */
PHP
    printf '%s\n' "$body" > "${dir}/inc/thing.php"
    printf '{"name":"smoke/composer-smoke"}\n' > "${dir}/composer.json"
    : > "$log"

    local status=0
    (
        cd "$dir"
        PATH="${fake_bin}:$PATH" \
        COMPOSER_INVOCATION_LOG="$log" \
        HOMEBOY_BUILD_SOURCE_DISPOSABLE="$disposable" \
        HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
        HOMEBOY_COMPONENT_ID="composer-smoke" \
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
        HOMEBOY_SKIP_TESTS=1 \
            bash "${EXTENSION_DIR}/scripts/build/build.sh"
    ) > "${TMP_DIR}/case-${case_index}/build.out" 2>&1 || status=$?

    local actual_exit=ok
    [ "$status" -eq 0 ] || actual_exit=fail
    local expected actual
    expected="$(printf '%s\n' "$@")"
    actual="$(cat "$log")"

    if [ "$actual_exit" = "$expected_exit" ] && [ "$actual" = "$expected" ]; then
        echo "PASS: ${label} ($(wc -l < "$log" | tr -d ' ') composer invocation(s))"
    else
        echo "FAIL: ${label}: exit ${actual_exit} (expected ${expected_exit})" >&2
        echo "  expected composer invocations:" >&2
        printf '    %s\n' "$@" >&2
        echo "  actual composer invocations:" >&2
        sed 's/^/    /' "$log" >&2
        sed 's/^/  | /' "${TMP_DIR}/case-${case_index}/build.out" >&2
        fail=1
    fi
}

check_case "persistent checkout build" 0 "$VALID_PHP" ok "$PROD_INSTALL" "$DEV_RESTORE"
check_case "persistent checkout lint failure" 0 "$BROKEN_PHP" fail "$PROD_INSTALL" "$DEV_RESTORE"
check_case "disposable source build" 1 "$VALID_PHP" ok "$PROD_INSTALL"
check_case "disposable source lint failure" 1 "$BROKEN_PHP" fail "$PROD_INSTALL"

if [ "$fail" -ne 0 ]; then
    echo "composer invocation checks FAILED" >&2
    exit 1
fi
echo "All composer invocation checks passed."
