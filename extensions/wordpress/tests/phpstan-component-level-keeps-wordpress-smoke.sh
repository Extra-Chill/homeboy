#!/usr/bin/env bash
set -euo pipefail

# Regression: a component that sets its own `level:` must keep the extension's
# WordPress environment.
#
# The runner used to read `level:` as a request for a clean slate and skip its
# own config entirely, which took phpstan-wordpress, the WordPress/WP-CLI
# stubs, dynamicConstantNames and the shared ignoreErrors with it. Components
# compensated by adding `includes: vendor/szepeviktor/...` — a path into a
# gitignored directory — and wherever that vendor tree was absent PHPStan ran
# with no WordPress signatures and reported correct core calls as defects:
#
#   Function esc_html_e invoked with 2 parameters, 1 required
#
# The fixture below calls esc_html_e() with its real two arguments and sets
# level: 5. It must analyse clean, because the extension config is included
# regardless of the level declaration.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHPSTAN_RUNNER="${ROOT_DIR}/scripts/lint/phpstan-runner.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

COMPONENT_DIR="${TMP_ROOT}/component"
CONFIG_TMPDIR="${TMP_ROOT}/generated-config"
OUTPUT_FILE="${TMP_ROOT}/phpstan-output.txt"
RESOLVE_CONTEXT_HELPER="${TMP_ROOT}/resolve-context.sh"
SIDECAR_WRITER_HELPER="${TMP_ROOT}/sidecar-writer.sh"

mkdir -p "${COMPONENT_DIR}/inc" "$CONFIG_TMPDIR"

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-phpstan-component-level}"
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
 * Plugin Name: PHPStan Component Level Fixture
 */
PHP

# Real core signatures: esc_html_e( $text, $domain ), current_time( $type, $gmt ).
# Without the extension's WordPress stubs these are reported as arity errors.
cat > "${COMPONENT_DIR}/inc/render.php" <<'PHP'
<?php

function fixture_render_label(): void {
    esc_html_e( 'Label', 'fixture-domain' );
}

function fixture_now(): string {
    return (string) current_time( 'mysql', true );
}
PHP

# The component sets its own level. That must not cost it WordPress.
cat > "${COMPONENT_DIR}/phpstan.neon" <<'NEON'
parameters:
    level: 5
    paths:
        - inc
NEON

if [ ! -x "${ROOT_DIR}/vendor/bin/phpstan" ]; then
    echo "FAIL: PHPStan is not installed. Run composer install in ${ROOT_DIR}." >&2
    exit 1
fi

set +e
HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpstan-component-level" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_PHPSTAN_THREADS=1 \
TMPDIR="$CONFIG_TMPDIR" \
bash -c 'cd "$1" && "$2"' _ "$CONFIG_TMPDIR" "$PHPSTAN_RUNNER" >"$OUTPUT_FILE" 2>&1
exit_code=$?
set -e

# Two ways the missing environment shows up, depending on whether any partial
# stub source happened to be reachable: the function is unknown entirely, or it
# is known with a truncated signature and correct calls look over-supplied.
if grep -Eq 'Function (esc_html_e|current_time) not found' "$OUTPUT_FILE"; then
    echo "FAIL: WordPress functions were unknown — a component-declared level dropped the extension config" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

if grep -Eq '(esc_html_e|current_time) invoked with' "$OUTPUT_FILE"; then
    echo "FAIL: WordPress signatures were truncated — correct core calls reported as arity errors" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

if [ "$exit_code" -ne 0 ]; then
    echo "FAIL: analysis of a level-declaring component should succeed" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

# The component's level must still win over the extension's.
if ! grep -Eq '"phpstan_level"[[:space:]]*:[[:space:]]*"(config|5)"' "$OUTPUT_FILE"; then
    echo "NOTE: could not confirm the component level from runner output" >&2
fi

echo "PASS: a component-declared level keeps the extension's WordPress environment"
