#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/phpstan-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component"
ARGS_FILE="${TMPDIR}/phpstan-args.txt"
CALLS_FILE="${TMPDIR}/phpstan-calls.txt"
DEPENDENCY_HELPER="${TMPDIR}/validation-dependencies.sh"
CONFIG_CAPTURE="${TMPDIR}/phpstan-config-capture.neon"
AUTOLOAD_CAPTURE="${TMPDIR}/phpstan-autoload-capture.php"

mkdir -p "${EXTENSION_DIR}/vendor/bin" "${COMPONENT_DIR}/tests" "${COMPONENT_DIR}/assets" "${COMPONENT_DIR}/includes" "${COMPONENT_DIR}/vendor_prefixed"
touch "${EXTENSION_DIR}/phpstan.neon.dist"
touch "${COMPONENT_DIR}/main.php" "${COMPONENT_DIR}/tests/FooTest.php" "${COMPONENT_DIR}/assets/app.js"
touch "${COMPONENT_DIR}/includes/interface-example.php" "${COMPONENT_DIR}/vendor_prefixed/autoload.php"

cat > "$DEPENDENCY_HELPER" <<'SH'
homeboy_resolve_validation_dependency_paths() {
    return 0
}
SH

cat > "${EXTENSION_DIR}/vendor/bin/phpstan" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" > "${PHPSTAN_ARGS_FILE}"
for arg in "$@"; do
    case "$arg" in
        --configuration=*) cp "${arg#--configuration=}" "${PHPSTAN_CONFIG_CAPTURE}" ;;
        --autoload-file=*) cp "${arg#--autoload-file=}" "${PHPSTAN_AUTOLOAD_CAPTURE}" ;;
    esac
done
count=0
[ -f "${PHPSTAN_CALLS_FILE}" ] && count=$(cat "${PHPSTAN_CALLS_FILE}")
count=$((count + 1))
printf '%s\n' "$count" > "${PHPSTAN_CALLS_FILE}"
printf '%s\n' '{"totals":{"errors":0,"file_errors":0},"files":{}}'
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpstan"

run_phpstan() {
    : > "$ARGS_FILE"
    printf '0\n' > "$CALLS_FILE"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="phpstan-smoke" \
    HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
    PHPSTAN_ARGS_FILE="$ARGS_FILE" \
    PHPSTAN_CALLS_FILE="$CALLS_FILE" \
    PHPSTAN_CONFIG_CAPTURE="$CONFIG_CAPTURE" \
    PHPSTAN_AUTOLOAD_CAPTURE="$AUTOLOAD_CAPTURE" \
    HOMEBOY_SUMMARY_MODE=1 \
    "$RUNNER" >/dev/null
}

assert_contains() {
    local needle="$1"
    local message="$2"
    if ! grep -F -- "$needle" "$ARGS_FILE" >/dev/null; then
        echo "FAIL: $message" >&2
        echo "Args: $(cat "$ARGS_FILE")" >&2
        exit 1
    fi
}

assert_not_contains() {
    local needle="$1"
    local message="$2"
    if grep -F -- "$needle" "$ARGS_FILE" >/dev/null; then
        echo "FAIL: $message" >&2
        echo "Args: $(cat "$ARGS_FILE")" >&2
        exit 1
    fi
}

assert_file_contains() {
    local file="$1"
    local needle="$2"
    local message="$3"
    if ! grep -F -- "$needle" "$file" >/dev/null; then
        echo "FAIL: $message" >&2
        echo "File: $file" >&2
        cat "$file" >&2
        exit 1
    fi
}

assert_file_not_contains() {
    local file="$1"
    local needle="$2"
    local message="$3"
    if grep -F -- "$needle" "$file" >/dev/null; then
        echo "FAIL: $message" >&2
        echo "File: $file" >&2
        cat "$file" >&2
        exit 1
    fi
}

HOMEBOY_LINT_FILE="main.php" run_phpstan
assert_contains "${COMPONENT_DIR}/main.php" "single-file scope passes the requested PHP file to PHPStan"
assert_not_contains "$COMPONENT_DIR " "single-file scope does not pass the whole component root"

if [ ! -f "$CONFIG_CAPTURE" ]; then
    echo "FAIL: scoped PHPStan run should generate a context config" >&2
    exit 1
fi

if [ ! -f "$AUTOLOAD_CAPTURE" ]; then
    echo "FAIL: scoped PHPStan run should generate a composite autoload file" >&2
    exit 1
fi

assert_file_contains "$CONFIG_CAPTURE" "${COMPONENT_DIR}/includes" "scoped context scans plugin production declarations"
assert_file_contains "$CONFIG_CAPTURE" "${COMPONENT_DIR}/vendor_prefixed" "scoped context scans prefixed vendor declarations"
assert_file_contains "$CONFIG_CAPTURE" "${COMPONENT_DIR}/main.php" "scoped context scans top-level plugin files"
assert_file_not_contains "$CONFIG_CAPTURE" "${COMPONENT_DIR}/tests" "scoped context excludes test declarations"
assert_file_contains "$AUTOLOAD_CAPTURE" "${COMPONENT_DIR}/vendor_prefixed/autoload.php" "composite autoload loads prefixed vendor autoloader"

HOMEBOY_LINT_GLOB='{main.php,assets/app.js,tests/FooTest.php}' run_phpstan
assert_contains "${COMPONENT_DIR}/main.php" "glob scope includes matching PHP source file"
assert_contains "${COMPONENT_DIR}/tests/FooTest.php" "glob scope includes matching PHP test file"
assert_not_contains "assets/app.js" "glob scope ignores non-PHP files"

: > "$ARGS_FILE"
printf '0\n' > "$CALLS_FILE"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpstan-smoke" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
PHPSTAN_ARGS_FILE="$ARGS_FILE" \
PHPSTAN_CALLS_FILE="$CALLS_FILE" \
PHPSTAN_CONFIG_CAPTURE="$CONFIG_CAPTURE" \
PHPSTAN_AUTOLOAD_CAPTURE="$AUTOLOAD_CAPTURE" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_LINT_FILE="assets/app.js" \
"$RUNNER" >/dev/null

if [ "$(cat "$CALLS_FILE")" != "0" ]; then
    echo "FAIL: non-PHP single-file scope should skip PHPStan" >&2
    exit 1
fi

echo "PHPStan scoped lint smoke passed"
