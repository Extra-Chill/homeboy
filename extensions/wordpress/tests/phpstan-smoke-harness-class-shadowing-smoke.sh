#!/usr/bin/env bash
set -euo pipefail

# Regression: class declarations in standalone smoke harnesses must not shadow
# the real classes that component source analyses against.
#
# Smoke harnesses (`tests/*-smoke.php`, `tests/smoke-*.php`) run under plain
# `php` without WordPress or Composer, so they idiomatically declare bare
# stand-ins for the classes they touch:
#
#     class WP_Agent_Execution_Principal {}
#
# PHPStan analyses the whole component (`paths: %currentWorkingDirectory%`),
# and a declaration found in analysed project source outranks the same class
# resolved through the Composer autoloader. The empty stand-in then wins, and
# every real member access in production code is reported as a defect:
#
#     Access to an undefined property WP_Agent_Execution_Principal::$acting_user_id.
#
# Scoped lint reports only the requested file, but discovery still covers the
# whole tree, so the bogus findings surface on any PR that touches the file.
# See Extra-Chill/data-machine#3474 and #3452.
#
# The fixture below mirrors that: a Composer-autoloaded class with a real
# property, a smoke harness declaring an empty stand-in of the same class, and
# production code reading the property. It must analyse clean.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHPSTAN_RUNNER="${ROOT_DIR}/scripts/lint/phpstan-runner.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

COMPONENT_DIR="${TMP_ROOT}/component"
CONFIG_TMPDIR="${TMP_ROOT}/generated-config"
OUTPUT_FILE="${TMP_ROOT}/phpstan-output.txt"
RESOLVE_CONTEXT_HELPER="${TMP_ROOT}/resolve-context.sh"
SIDECAR_WRITER_HELPER="${TMP_ROOT}/sidecar-writer.sh"

mkdir -p \
    "${COMPONENT_DIR}/inc" \
    "${COMPONENT_DIR}/tests" \
    "${COMPONENT_DIR}/vendor/acme/principal/src" \
    "${COMPONENT_DIR}/vendor/composer" \
    "$CONFIG_TMPDIR"

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-phpstan-smoke-harness-shadowing}"
}
SH

cat > "$SIDECAR_WRITER_HELPER" <<'SH'
homeboy_sidecar_merge_json_array() {
    return 0
}
SH

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: PHPStan Smoke Harness Shadowing Fixture
 */
PHP

# The real class, delivered by Composer like a vendored library.
cat > "${COMPONENT_DIR}/vendor/acme/principal/src/Fixture_Principal.php" <<'PHP'
<?php
namespace Acme\Principal;

final class Fixture_Principal {
    public function __construct( public readonly int $acting_user_id ) {}

    public function describe(): string {
        return 'user:' . $this->acting_user_id;
    }
}
PHP

# A genuine Composer classmap autoloader, generated from composer.json, so the
# class reaches PHPStan exactly as a vendored library would.
cat > "${COMPONENT_DIR}/vendor/acme/principal/composer.json" <<'JSON'
{ "name": "acme/principal", "autoload": { "classmap": [ "src/" ] } }
JSON
cat > "${COMPONENT_DIR}/composer.json" <<'JSON'
{
    "name": "fixture/component",
    "repositories": [ { "type": "path", "url": "vendor-src/principal", "options": { "symlink": false } } ],
    "autoload": { "classmap": [ "vendor/acme/principal/src/" ] }
}
JSON

# Production code using the real members.
cat > "${COMPONENT_DIR}/inc/actor.php" <<'PHP'
<?php

function fixture_actor_user_id( \Acme\Principal\Fixture_Principal $principal ): int {
    return $principal->acting_user_id;
}

function fixture_actor_label( \Acme\Principal\Fixture_Principal $principal ): string {
    return $principal->describe();
}
PHP

# The standalone smoke harness declares an empty stand-in so it can run under
# plain `php`. This must not become the class PHPStan analyses against.
cat > "${COMPONENT_DIR}/tests/actor-smoke.php" <<'PHP'
<?php
namespace Acme\Principal {
    final class Fixture_Principal {}
}

namespace {
    echo "smoke\n";
}
PHP

( cd "$COMPONENT_DIR" && composer dump-autoload --no-interaction --quiet ) || {
    echo "FAIL: composer dump-autoload failed for the fixture" >&2
    exit 1
}

if [ ! -x "${ROOT_DIR}/vendor/bin/phpstan" ]; then
    echo "FAIL: PHPStan is not installed. Run composer install in ${ROOT_DIR}." >&2
    exit 1
fi

run_runner() {
    set +e
    HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="phpstan-smoke-harness-shadowing" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
    HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
    HOMEBOY_SUMMARY_MODE=1 \
    HOMEBOY_PHPSTAN_THREADS=1 \
    TMPDIR="$CONFIG_TMPDIR" \
    "$@" \
    bash -c 'cd "$1" && "$2"' _ "$CONFIG_TMPDIR" "$PHPSTAN_RUNNER" >"$OUTPUT_FILE" 2>&1
    local code=$?
    set -e
    return "$code"
}

assert_clean() {
    local label="$1"
    local exit_code="$2"

    if grep -Eq 'Fixture_Principal::\$acting_user_id|Fixture_Principal::describe\(\)|Fixture_Principal does not have a constructor' "$OUTPUT_FILE"; then
        echo "FAIL (${label}): a smoke-harness stand-in shadowed the autoloaded class" >&2
        cat "$OUTPUT_FILE" >&2
        exit 1
    fi

    if [ "$exit_code" -ne 0 ]; then
        echo "FAIL (${label}): analysis should succeed" >&2
        cat "$OUTPUT_FILE" >&2
        exit 1
    fi
}

# Full-component analysis.
exit_code=0
run_runner env || exit_code=$?
assert_clean "full" "$exit_code"

# Changed-file scoped analysis: only the production file is reported, but
# discovery must still exclude the harness declaration.
exit_code=0
run_runner env HOMEBOY_LINT_FILE="inc/actor.php" || exit_code=$?
assert_clean "scoped" "$exit_code"

echo "PASS: smoke-harness class stand-ins do not shadow autoloaded classes"
