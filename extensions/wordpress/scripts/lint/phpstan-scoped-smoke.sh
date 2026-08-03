#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
RESOLVE_CONTEXT_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
RUNNER="${SCRIPT_DIR}/phpstan-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

if [ ! -f "$SIDECAR_WRITER_HELPER" ]; then
    echo "Missing sidecar writer helper: $SIDECAR_WRITER_HELPER" >&2
    exit 1
fi

if [ ! -f "$RESOLVE_CONTEXT_HELPER" ]; then
    echo "Missing resolve context helper: $RESOLVE_CONTEXT_HELPER" >&2
    exit 1
fi

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component"
ARGS_FILE="${TMPDIR}/phpstan-args.txt"
CALLS_FILE="${TMPDIR}/phpstan-calls.txt"
DEPENDENCY_HELPER="${TMPDIR}/validation-dependencies.sh"
CONFIG_CAPTURE="${TMPDIR}/phpstan-config-capture.neon"
AUTOLOAD_CAPTURE="${TMPDIR}/phpstan-autoload-capture.php"
OUTPUT_FILE="${TMPDIR}/phpstan-output.txt"
FINDINGS_FILE="${TMPDIR}/phpstan-findings.json"
PRODUCER_METADATA_FILE="${TMPDIR}/phpstan-producer-metadata.json"

mkdir -p \
    "${EXTENSION_DIR}/vendor/bin" \
    "${COMPONENT_DIR}/tests" \
    "${COMPONENT_DIR}/assets" \
    "${COMPONENT_DIR}/includes" \
    "${COMPONENT_DIR}/vendor_prefixed" \
    "${COMPONENT_DIR}/.homeboy-build/static-site-importer/includes"
touch "${EXTENSION_DIR}/phpstan.neon.dist"
printf '%s\n' '<?php missing_function();' > "${COMPONENT_DIR}/main.php"
printf '%s\n' 'parameters:' '    ignoreErrors: []' > "${COMPONENT_DIR}/phpstan-baseline.neon"
touch "${COMPONENT_DIR}/tests/FooTest.php" "${COMPONENT_DIR}/assets/app.js"
touch "${COMPONENT_DIR}/includes/interface-example.php" "${COMPONENT_DIR}/includes/extra.php" "${COMPONENT_DIR}/vendor_prefixed/autoload.php"
touch "${COMPONENT_DIR}/.homeboy-build/static-site-importer/includes/stale-copy.php"

cat > "$DEPENDENCY_HELPER" <<'SH'
homeboy_resolve_validation_dependency_paths() {
    return 0
}
SH

cat > "${EXTENSION_DIR}/vendor/bin/phpstan" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" > "${PHPSTAN_ARGS_FILE}"
count=0
[ -f "${PHPSTAN_CALLS_FILE}" ] && count=$(cat "${PHPSTAN_CALLS_FILE}")
count=$((count + 1))
printf '%s\n' "$count" > "${PHPSTAN_CALLS_FILE}"
if [ "${1:-}" = "--version" ]; then
    printf '%s\n' 'PHPStan - PHP Static Analysis Tool 2.0.0'
    exit 0
fi
for arg in "$@"; do
    case "$arg" in
        --configuration=*) cp "${arg#--configuration=}" "${PHPSTAN_CONFIG_CAPTURE}" ;;
        --autoload-file=*) cp "${arg#--autoload-file=}" "${PHPSTAN_AUTOLOAD_CAPTURE}" ;;
    esac
done
if [ "${PHPSTAN_EMIT_ERROR:-}" = "1" ]; then
    printf '{"totals":{"errors":1,"file_errors":1},"files":{"%s/main.php":{"errors":1,"messages":[{"message":"Call to an undefined function missing_function().","line":1,"identifier":"function.notFound"}]}}}\n' "${HOMEBOY_COMPONENT_PATH}"
    exit 1
fi
printf '%s\n' '{"totals":{"errors":0,"file_errors":0},"files":{}}'
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpstan"

run_phpstan() {
    : > "$ARGS_FILE"
    : > "$OUTPUT_FILE"
    printf '0\n' > "$CALLS_FILE"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="phpstan-smoke" \
    HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
    HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
    PHPSTAN_ARGS_FILE="$ARGS_FILE" \
    PHPSTAN_CALLS_FILE="$CALLS_FILE" \
    PHPSTAN_CONFIG_CAPTURE="$CONFIG_CAPTURE" \
    PHPSTAN_AUTOLOAD_CAPTURE="$AUTOLOAD_CAPTURE" \
    HOMEBOY_SUMMARY_MODE=1 \
    "$RUNNER" >"$OUTPUT_FILE"
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
assert_contains "--level=7" "PHPStan defaults to the same level declared by wordpress/phpstan.neon.dist"
assert_not_contains "--baseline" "PHPStan 2.x removed the --baseline CLI flag"

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

run_phpstan
assert_contains "--level=7" "full-component PHPStan run defaults to level 7"
assert_not_contains "--baseline" "full-component PHPStan run avoids removed --baseline flag"
assert_file_contains "$CONFIG_CAPTURE" "${EXTENSION_DIR}/phpstan.neon.dist" "full-component PHPStan config includes the extension default config"
assert_file_contains "$CONFIG_CAPTURE" "${COMPONENT_DIR}/phpstan-baseline.neon" "full-component PHPStan config includes component baseline via neon"
assert_file_not_contains "$OUTPUT_FILE" "ERRORS (raw)" "zero-error PHPStan JSON should not be printed as raw errors"

printf '%s\n' 'parameters:' '    scanFiles:' '        - stubs/example.php' > "${COMPONENT_DIR}/phpstan.neon.dist"
run_phpstan
assert_contains "--level=7" "partial component PHPStan config still uses the default level"
assert_file_contains "$CONFIG_CAPTURE" "${EXTENSION_DIR}/phpstan.neon.dist" "partial component-config run includes the extension default config"
assert_file_contains "$CONFIG_CAPTURE" "${COMPONENT_DIR}/phpstan.neon.dist" "partial component-config run includes the component config"

printf '%s\n' 'parameters:' '    level: max' > "${COMPONENT_DIR}/phpstan.neon.dist"
run_phpstan
assert_not_contains "--level=7" "component PHPStan config controls level when env override is absent"
assert_file_not_contains "$CONFIG_CAPTURE" "${EXTENSION_DIR}/phpstan.neon.dist" "component-config run does not duplicate the extension default config"
assert_file_contains "$CONFIG_CAPTURE" "${COMPONENT_DIR}/phpstan.neon.dist" "component-config run includes the component config"
assert_file_contains "${COMPONENT_DIR}/phpstan.neon.dist" "level: max" "component-config fixture preserves the component ruleset"

HOMEBOY_PHPSTAN_LEVEL=5 run_phpstan
assert_contains "--level=5" "HOMEBOY_PHPSTAN_LEVEL explicitly overrides component PHPStan config level"
assert_file_contains "$CONFIG_CAPTURE" "${COMPONENT_DIR}/phpstan.neon.dist" "env override still uses component PHPStan config"

HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE="$PRODUCER_METADATA_FILE" HOMEBOY_PHPSTAN_LEVEL=5 run_phpstan
python3 - "$PRODUCER_METADATA_FILE" "$COMPONENT_DIR" <<'PY'
import json
import sys

metadata = json.load(open(sys.argv[1], encoding="utf-8"))
component_dir = sys.argv[2]
assert metadata["phpstan_config_source"] == "component-local", metadata
assert metadata["phpstan_component_config"] == f"{component_dir}/phpstan.neon.dist", metadata
assert metadata["phpstan_level"] == "5", metadata
assert metadata["phpstan_level_source"] == "env", metadata
PY
rm -f "${COMPONENT_DIR}/phpstan.neon.dist"

HOMEBOY_PHP_VERSION=8.1 run_phpstan
assert_contains "--configuration=" "release-style PHP version run still passes a generated PHPStan config"
assert_file_contains "$CONFIG_CAPTURE" "maximumNumberOfProcesses: 1" "release-style generated dependency config forces single-process PHPStan by default"
assert_file_contains "$CONFIG_CAPTURE" "phpVersion: 80100" "release-style generated dependency config preserves PHP version override"
assert_not_contains ".homeboy-build" "full-component PHPStan run excludes Homeboy release build artifacts"
assert_file_not_contains "$CONFIG_CAPTURE" ".homeboy-build" "release-style PHPStan context excludes Homeboy release build artifacts"

HOMEBOY_LINT_GLOB='{main.php,assets/app.js,includes/extra.php}' run_phpstan
assert_contains "${COMPONENT_DIR}/main.php" "glob scope includes matching PHP source file"
assert_contains "${COMPONENT_DIR}/includes/extra.php" "glob scope includes matching PHP runtime file"
assert_not_contains "assets/app.js" "glob scope ignores non-PHP files"
assert_file_contains "$OUTPUT_FILE" "PHPStan scoped lint: analyzing 2 PHP file(s)" "relative glob scope reports analyzed PHP files"

# A glob that resolves to a directory expands through a find, which is the one
# scope that walks whatever the directory happens to contain. An installed
# dependency tree ships PHP of its own, and analyzing it is neither the
# component's code nor the component's problem.
mkdir -p "${COMPONENT_DIR}/includes/node_modules/example-package"
printf '%s\n' '<?php missing_function();' > "${COMPONENT_DIR}/includes/node_modules/example-package/shim.php"
HOMEBOY_LINT_GLOB='{includes,main.php}' run_phpstan
assert_contains "${COMPONENT_DIR}/includes/extra.php" "directory glob scope includes component PHP files"
assert_not_contains "node_modules" "directory glob scope excludes the installed dependency tree"
rm -rf "${COMPONENT_DIR}/includes/node_modules"

HOMEBOY_LINT_GLOB="{${COMPONENT_DIR}/main.php,${COMPONENT_DIR}/assets/app.js,${COMPONENT_DIR}/includes/extra.php}" run_phpstan
assert_contains "${COMPONENT_DIR}/main.php" "absolute glob scope includes matching PHP source file"
assert_contains "${COMPONENT_DIR}/includes/extra.php" "absolute glob scope includes matching PHP runtime file"
assert_not_contains "assets/app.js" "absolute glob scope ignores non-PHP files"
assert_file_contains "$OUTPUT_FILE" "PHPStan scoped lint: analyzing 2 PHP file(s)" "absolute glob scope reports analyzed PHP files"

: > "$ARGS_FILE"
printf '0\n' > "$CALLS_FILE"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpstan-smoke" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
PHPSTAN_ARGS_FILE="$ARGS_FILE" \
PHPSTAN_CALLS_FILE="$CALLS_FILE" \
PHPSTAN_CONFIG_CAPTURE="$CONFIG_CAPTURE" \
PHPSTAN_AUTOLOAD_CAPTURE="$AUTOLOAD_CAPTURE" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_LINT_FILE="assets/app.js" \
"$RUNNER" >"$OUTPUT_FILE"

if [ "$(cat "$CALLS_FILE")" != "1" ]; then
    echo "FAIL: non-PHP single-file scope should only run the PHPStan integrity probe" >&2
    exit 1
fi

assert_contains "--version" "non-PHP single-file scope runs the PHPStan integrity probe"
assert_not_contains "analyse" "non-PHP single-file scope skips PHPStan analysis"
assert_file_contains "$OUTPUT_FILE" "no PHP files in requested scope, skipping static analysis" "non-PHP single-file scope reports that PHPStan analysis was skipped"

: > "$ARGS_FILE"
printf '0\n' > "$CALLS_FILE"
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpstan-smoke" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
PHPSTAN_ARGS_FILE="$ARGS_FILE" \
PHPSTAN_CALLS_FILE="$CALLS_FILE" \
PHPSTAN_CONFIG_CAPTURE="$CONFIG_CAPTURE" \
PHPSTAN_AUTOLOAD_CAPTURE="$AUTOLOAD_CAPTURE" \
PHPSTAN_EMIT_ERROR=1 \
_HOMEBOY_PHPSTAN_FINDINGS_FILE="$FINDINGS_FILE" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_SUMMARY_MODE=1 \
"$RUNNER" >"$OUTPUT_FILE"
phpstan_error_status=$?
set -e

if [ "$phpstan_error_status" -eq 0 ]; then
    echo "FAIL: PHPStan error fixture should fail" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

python3 - "$FINDINGS_FILE" <<'PY'
import json
import sys

findings = json.load(open(sys.argv[1], encoding="utf-8"))
assert len(findings) == 1, findings
finding = findings[0]
expected = {
    "tool": "phpstan",
    "file": "main.php",
    "line": 1,
    "column": None,
    "severity": "error",
    "code": "phpstan.function.notFound",
    "rule": "phpstan.function.notFound",
    "category": "phpstan",
    "fixable": False,
    "excerpt": "<?php missing_function();",
}
for key, value in expected.items():
    assert finding.get(key) == value, (key, finding)
assert "source" not in finding, finding
assert finding.get("fingerprint"), finding
PY

echo "PHPStan scoped lint smoke passed"
