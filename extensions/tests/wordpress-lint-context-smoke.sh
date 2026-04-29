#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="$ROOT_DIR/wordpress/scripts/lint/lint-runner.sh"
PHPSTAN_RUNNER="$ROOT_DIR/wordpress/scripts/lint/phpstan-runner.sh"
PHPSTAN_CONFIG="$ROOT_DIR/wordpress/phpstan.neon.dist"

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq -- "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        exit 1
    fi
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

COMPONENT_DIR="$TMP_DIR/example-plugin"
FAKE_EXTENSION="$TMP_DIR/fake-wordpress-extension"
mkdir -p "$COMPONENT_DIR/tools" "$COMPONENT_DIR/tests" "$COMPONENT_DIR/vendor_prefixed/example" "$FAKE_EXTENSION/vendor/bin" "$FAKE_EXTENSION/HomeboyWordPress"

cat > "$COMPONENT_DIR/example-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Example Plugin
 * Text Domain: example-plugin
 */
PHP

touch "$FAKE_EXTENSION/vendor/bin/phpcs" "$FAKE_EXTENSION/vendor/bin/phpcbf" "$FAKE_EXTENSION/phpcs.xml.dist"

cat > "$COMPONENT_DIR/scoper.inc.php" <<'PHP'
<?php
return [
    'prefix' => 'Example\\Vendor',
];
PHP

cat > "$COMPONENT_DIR/tools/build-autoloader.php" <<'PHP'
<?php
file_put_contents(__DIR__ . '/autoload.php', '<?php return [];');
PHP

cat > "$COMPONENT_DIR/tests/smoke-example.php" <<'PHP'
<?php
require_once __DIR__ . '/../example-plugin.php';
echo "ok\n";
PHP

cat > "$COMPONENT_DIR/tests/ExampleUnitTest.php" <<'PHP'
<?php
class ExampleUnitTest extends WP_UnitTestCase {}
PHP

cat > "$COMPONENT_DIR/vendor_prefixed/example/generated.php" <<'PHP'
<?php
class Example_Generated {}
PHP

HOMEBOY_EXTENSION_PATH="$FAKE_EXTENSION" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="example-plugin" \
HOMEBOY_STEP="none" \
    bash "$RUNNER" > "$TMP_DIR/lint.out" 2>&1

assert_contains "$TMP_DIR/lint.out" "Linting passed"

assert_contains "$RUNNER" 'homeboy_resolve_context --component-alias PLUGIN_PATH'
assert_contains "$RUNNER" "*/vendor_prefixed/*"
assert_contains "$RUNNER" "*/tools/*"
assert_contains "$RUNNER" "*/scoper.inc.php"
assert_contains "$PHPSTAN_RUNNER" "*/vendor_prefixed/*"
assert_contains "$PHPSTAN_RUNNER" "*/tools/*"
assert_contains "$PHPSTAN_RUNNER" "scoper.inc.php"
assert_contains "$PHPSTAN_CONFIG" "*/vendor_prefixed/*"
assert_contains "$PHPSTAN_CONFIG" "*/tools/*"
assert_contains "$PHPSTAN_CONFIG" "*/scoper.inc.php"

for non_runtime_file in \
    scoper.inc.php \
    tools/build-autoloader.php \
    tests/smoke-example.php \
    tests/ExampleUnitTest.php \
    vendor_prefixed/example/generated.php; do
    HOMEBOY_EXTENSION_PATH="$FAKE_EXTENSION" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="example-plugin" \
    HOMEBOY_LINT_FILE="$non_runtime_file" \
        bash "$RUNNER" > "$TMP_DIR/non-runtime.out" 2>&1

    assert_contains "$TMP_DIR/non-runtime.out" "Skipping production WordPress lint profile for non-runtime file scope"
    assert_contains "$TMP_DIR/non-runtime.out" "Linting passed"
done

mkdir -p "$COMPONENT_DIR/includes" "$COMPONENT_DIR/vendor_prefixed/Example/Vendor"

cat > "$COMPONENT_DIR/includes/interface-example-adapter.php" <<'PHP'
<?php
interface Example_Adapter {}
PHP

cat > "$COMPONENT_DIR/includes/class-example-adapter.php" <<'PHP'
<?php
class Example_Adapter_Impl implements Example_Adapter {}
PHP

cat > "$COMPONENT_DIR/vendor_prefixed/Example/Vendor/class-prefixed-dependency.php" <<'PHP'
<?php
namespace Example\Vendor;
class Prefixed_Dependency {}
PHP

cat > "$COMPONENT_DIR/vendor_prefixed/autoload.php" <<'PHP'
<?php
// Fake scoped Composer autoloader.
PHP

cat > "$FAKE_EXTENSION/phpstan.neon.dist" <<'NEON'
parameters:
    level: 7
NEON

cat > "$FAKE_EXTENSION/vendor/bin/phpstan" <<'BASH'
#!/usr/bin/env bash
set -euo pipefail

config=""
autoload_file=""
targets=()
for arg in "$@"; do
    case "$arg" in
        --configuration=*) config="${arg#--configuration=}" ;;
        --autoload-file=*) autoload_file="${arg#--autoload-file=}" ;;
        --*) ;;
        analyse) ;;
        *) targets+=("$arg") ;;
    esac
done

if [ -z "$config" ] || [ ! -f "$config" ]; then
    echo "missing generated PHPStan config" >&2
    exit 1
fi

if [ "${#targets[@]}" -ne 1 ] || [ "${targets[0]}" != "${EXPECTED_TARGET}" ]; then
    printf 'unexpected PHPStan targets: %s\n' "${targets[*]:-<none>}" >&2
    exit 1
fi

grep -Fq "$EXPECTED_INTERFACE" "$config" || {
    echo "scoped config did not include sibling production file context" >&2
    exit 1
}

grep -Fq "$EXPECTED_VENDOR_PREFIXED" "$config" || {
    echo "scoped config did not include vendor_prefixed dependency context" >&2
    exit 1
}

if [ -z "$autoload_file" ] || ! grep -Fq "$EXPECTED_VENDOR_PREFIXED_AUTOLOAD" "$autoload_file"; then
    echo "composite autoload did not include vendor_prefixed autoloader" >&2
    exit 1
fi

printf 'PHPStan fake passed\n'
BASH
chmod +x "$FAKE_EXTENSION/vendor/bin/phpstan"

EXPECTED_TARGET="$COMPONENT_DIR/includes/class-example-adapter.php" \
EXPECTED_INTERFACE="$COMPONENT_DIR/includes/interface-example-adapter.php" \
EXPECTED_VENDOR_PREFIXED="$COMPONENT_DIR/vendor_prefixed" \
EXPECTED_VENDOR_PREFIXED_AUTOLOAD="$COMPONENT_DIR/vendor_prefixed/autoload.php" \
HOMEBOY_EXTENSION_PATH="$FAKE_EXTENSION" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="example-plugin" \
HOMEBOY_LINT_FILE="includes/class-example-adapter.php" \
    bash "$PHPSTAN_RUNNER" > "$TMP_DIR/phpstan-scoped.out" 2>&1

assert_contains "$TMP_DIR/phpstan-scoped.out" "PHPStan scoped lint: analyzing 1 PHP file(s) from requested scope"
assert_contains "$TMP_DIR/phpstan-scoped.out" "PHPStan fake passed"

echo "wordpress lint context smoke passed"
