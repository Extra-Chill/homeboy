#!/usr/bin/env bash
set -euo pipefail

# A standalone PHP smoke must receive the validation dependencies its component
# declared. Without this, a cross-plugin test has no way to locate a declared
# checkout and its caller is pushed into supplying an ambient path.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

component="${TMPDIR}/component"
dependency="${TMPDIR}/declared-dependency"
runtime_stub="${TMPDIR}/runtime.sh"
mkdir -p "${component}/tests" "${dependency}"

# A plugin-shaped dependency so slug resolution has something real to read.
printf '%s\n' '<?php' '/**' ' * Plugin Name: Declared Dependency' ' */' \
    > "${dependency}/declared-dependency.php"

# The smoke asserts its own contract: the declared root reaches PHP by slug and
# in the roster, and it points at the directory the component declared.
cat > "${component}/tests/dependency-env-smoke.php" <<'PHP'
<?php
$expected = getenv( 'EXPECTED_DEPENDENCY_ROOT' );
$named    = getenv( 'DECLARED_DEPENDENCY_PATH' );
$roster   = json_decode( (string) getenv( 'HOMEBOY_WORDPRESS_DEPENDENCY_ROOTS_JSON' ), true );

if ( $named !== $expected ) {
    fwrite( STDERR, "DECLARED_DEPENDENCY_PATH was " . var_export( $named, true ) . "\n" );
    exit( 1 );
}
if ( ! is_array( $roster ) || ( $roster['declared-dependency'] ?? null ) !== $expected ) {
    fwrite( STDERR, "dependency roster was " . var_export( $roster, true ) . "\n" );
    exit( 1 );
}
echo "declared dependency reached the standalone smoke\n";
PHP

cat > "$runtime_stub" <<'SH'
#!/usr/bin/env bash
echo "PHPUNIT_RUNTIME_INVOKED"
SH
chmod +x "$runtime_stub"

cat > "${component}/homeboy-test-manifest.json" <<'JSON'
{
    "schema": "homeboy/test-manifest/v1",
    "tests": {
        "tests/dependency-env-smoke.php": {"environment": "standalone-php"}
    }
}
JSON

settings_json="$(jq -nc --arg path "$dependency" '{validation_dependencies: [{path: $path, plugin_slug: "declared-dependency"}]}')"

set +e
EXPECTED_DEPENDENCY_ROOT="$dependency" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="$runtime_stub" \
HOMEBOY_SETTINGS_JSON="$settings_json" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/run.out" 2>&1
status=$?
set -e

if [ "$status" -ne 0 ]; then
    echo "Standalone dependency environment run failed (exit ${status}):" >&2
    cat "${TMPDIR}/run.out" >&2
    exit 1
fi

grep -Fq "PHP_SMOKE_OK:tests/dependency-env-smoke.php" "${TMPDIR}/run.out"
grep -Fq "declared dependency reached the standalone smoke" "${TMPDIR}/run.out"
grep -Fq "Dependency: DECLARED_DEPENDENCY_PATH=${dependency}" "${TMPDIR}/run.out"

echo "Standalone dependency environment smoke passed"
