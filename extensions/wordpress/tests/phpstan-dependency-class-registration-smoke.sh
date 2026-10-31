#!/usr/bin/env bash
set -euo pipefail

# Regression: dependency classes must be registered for analysis even when the
# dependency ships no Composer autoloader.
#
# `scanFiles:` and `scanDirectories:` make declarations available for signature
# resolution but do not register a class for member access — PHPStan reports
# `unknown class` plus every property and method access on it. Only an
# autoloader registers classes.
#
# WordPress plugins commonly load classes with `require_once` from a bootstrap
# file rather than Composer, so such dependencies contribute no autoloader.
# Their file names also follow WordPress conventions rather than PSR-4, so the
# mapping has to be indexed from the declarations themselves.
#
# This asserts the generated composite autoloader carries a class map covering
# those dependencies, and that the indexer resolves fully-qualified names.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/lint/phpstan-runner.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

DEPENDENCY_DIR="${TMP_ROOT}/fixture-dependency"
DECLARATION_DIR="${TMP_ROOT}/declarations"
mkdir -p "${DEPENDENCY_DIR}/src/Identity" "${DEPENDENCY_DIR}/vendor/acme" "${DEPENDENCY_DIR}/tests"

cat > "${DEPENDENCY_DIR}/fixture-dependency.php" <<'PHP'
<?php
/**
 * Plugin Name: Fixture Dependency
 */
final class Fixture_Entrypoint_Class {
    public function identity(): string {
        return 'fixture';
    }
}

$GLOBALS['fixture_dependency_bootstrapped'] = true;
require_once 'core/abstraction.php';
PHP

# WordPress file naming, PSR-4-incompatible: no path convention derives the FQCN.
cat > "${DEPENDENCY_DIR}/src/Identity/class-fixture-identity.php" <<'PHP'
<?php
namespace FixtureAPI\Core\Identity;

final class Fixture_Materialized_Identity {
    public function __construct(
        public readonly int $id,
        public readonly string $scope,
    ) {}
}
PHP

# Declarations that must never enter the map.
cat > "${DEPENDENCY_DIR}/vendor/acme/vendored.php" <<'PHP'
<?php
namespace Acme;
class Vendored {}
PHP

cat > "${DEPENDENCY_DIR}/tests/dependency-own-smoke.php" <<'PHP'
<?php
namespace FixtureAPI\Core\Identity;
class Fixture_Materialized_Identity {}
PHP

# The runner is an executable script, so extract the helpers under test rather
# than sourcing it.
eval "$(awk '/^homeboy_resolve_phpstan_dependency_signature_files\(\)/,/^}/' "$RUNNER")"
eval "$(awk '/^homeboy_emit_dependency_class_map_entries\(\)/,/^}/' "$RUNNER")"

for helper in homeboy_resolve_phpstan_dependency_signature_files homeboy_emit_dependency_class_map_entries; do
    if ! declare -F "$helper" >/dev/null 2>&1; then
        echo "FAIL: ${helper}() is not defined" >&2
        exit 1
    fi
done

entries="$(homeboy_emit_dependency_class_map_entries "$DEPENDENCY_DIR" "$DECLARATION_DIR")"

if ! grep -qP '^FixtureAPI\\Core\\Identity\\Fixture_Materialized_Identity\t' <<< "$entries"; then
    echo "FAIL: namespaced dependency class must be indexed with its fully-qualified name" >&2
    printf '%s\n' "$entries" >&2
    exit 1
fi

mapped_file="$(grep -P '^FixtureAPI\\Core\\Identity\\Fixture_Materialized_Identity\t' <<< "$entries" | head -1 | cut -f2)"
if [[ "$mapped_file" != "${DECLARATION_DIR}/"*.php ]]; then
    echo "FAIL: class must map to a generated declaration file, got: ${mapped_file}" >&2
    exit 1
fi

entrypoint_file="$(grep -P '^Fixture_Entrypoint_Class\t' <<< "$entries" | head -1 | cut -f2)"
if [ ! -f "$entrypoint_file" ]; then
    echo "FAIL: entrypoint class must map to a generated declaration file" >&2
    printf '%s\n' "$entries" >&2
    exit 1
fi

# Loading a discovered class must not execute the dependency file. The original
# entrypoint has a relative require that deterministically fatals outside its
# runtime bootstrap context.
php -r 'require $argv[1]; $instance = new Fixture_Entrypoint_Class(); exit($instance->identity() === "fixture" && empty($GLOBALS["fixture_dependency_bootstrapped"]) ? 0 : 1);' "$entrypoint_file"

if grep -q 'Acme\\Vendored' <<< "$entries"; then
    echo "FAIL: vendored dependency classes must not be indexed" >&2
    exit 1
fi

if grep -qF "${DEPENDENCY_DIR}/tests/" <<< "$entries"; then
    echo "FAIL: the dependency's own tests must not be indexed" >&2
    exit 1
fi

# The generated autoloader must register the map for autoloader-less
# dependencies.
if ! grep -qF 'homeboy_emit_dependency_class_map_entries "$dependency_path"' "$RUNNER"; then
    echo "FAIL: generate_composite_autoload() must build a dependency class map" >&2
    exit 1
fi

if ! grep -qF 'spl_autoload_register' "$RUNNER"; then
    echo "FAIL: generate_composite_autoload() must register the class map" >&2
    exit 1
fi

echo "PHPStan dependency class registration smoke passed"
