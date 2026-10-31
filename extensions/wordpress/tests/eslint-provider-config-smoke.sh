#!/usr/bin/env bash
set -euo pipefail

EXTENSION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ESLINT_BIN="${EXTENSION_DIR}/node_modules/.bin/eslint"
ESLINT_CONFIG="${EXTENSION_DIR}/eslint.runner.config.mjs"
ESLINT_RUNNER="${EXTENSION_DIR}/scripts/lint/eslint-runner.sh"
FIXTURES_DIR="${EXTENSION_DIR}/tests/fixtures/eslint-provider"

if [ ! -x "$ESLINT_BIN" ]; then
    echo "Skipping: $ESLINT_BIN not found (run \`npm ci\` in $EXTENSION_DIR)" >&2
    exit 0
fi

print_config() {
    local component_path="$1"
    local output_file="$2"
    local source_file="${3:-src/admin.jsx}"

    (
        cd "$component_path"
        HOMEBOY_ESLINT_COMPONENT_PATH="$component_path" \
            "$ESLINT_BIN" --config "$ESLINT_CONFIG" \
            --print-config "$source_file" > "$output_file"
    )
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

print_config "${FIXTURES_DIR}/repository@feature" "${TMP_DIR}/worktree.json"
print_config "${FIXTURES_DIR}/repository" "${TMP_DIR}/primary.json"
print_config "${FIXTURES_DIR}/default-only" "${TMP_DIR}/default.json"
print_config "${FIXTURES_DIR}/precedence" "${TMP_DIR}/precedence.json" "src/admin.js"
for config_extension in js mjs cjs ts mts cts; do
    print_config \
        "${FIXTURES_DIR}/standard-${config_extension}" \
        "${TMP_DIR}/standard-${config_extension}.json" \
        "src/admin.js"
done
for extension in js ts tsx; do
    print_config \
        "${FIXTURES_DIR}/default-only" \
        "${TMP_DIR}/default-${extension}.json" \
        "src/admin.${extension}"
done

node - \
    "${TMP_DIR}/worktree.json" \
    "${TMP_DIR}/primary.json" \
    "${TMP_DIR}/default.json" \
    "${TMP_DIR}/precedence.json" \
    "${TMP_DIR}/standard-js.json" \
    "${TMP_DIR}/standard-mjs.json" \
    "${TMP_DIR}/standard-cjs.json" \
    "${TMP_DIR}/standard-ts.json" \
    "${TMP_DIR}/standard-mts.json" \
    "${TMP_DIR}/standard-cts.json" <<'JS'
const fs = require( 'fs' );

const readConfig = ( file ) => JSON.parse( fs.readFileSync( file, 'utf8' ) );
const worktree = readConfig( process.argv[ 2 ] );
const primary = readConfig( process.argv[ 3 ] );
const defaults = readConfig( process.argv[ 4 ] );
const precedence = readConfig( process.argv[ 5 ] );

if ( worktree.rules[ 'no-console' ][ 0 ] !== 0 ) {
  throw new Error( 'Expected the worktree JSX override to disable no-console' );
}
if ( primary.rules[ 'no-console' ][ 0 ] !== 2 ) {
  throw new Error( 'Expected the sibling primary fixture to enable no-console' );
}
if ( defaults.rules[ 'no-console' ][ 0 ] !== 1 ) {
  throw new Error( 'Expected default-only JSX to retain provider no-console' );
}
if ( defaults.rules[ 'no-undef' ][ 0 ] !== 2 ) {
  throw new Error( 'Expected default-only JSX to retain WordPress rules' );
}
if ( defaults.languageOptions.parserOptions.ecmaFeatures.jsx !== true ) {
  throw new Error( 'Expected provider defaults to parse discovered JSX' );
}
if ( precedence.rules[ 'no-console' ][ 0 ] !== 0 ) {
  throw new Error( 'Expected eslint.config.js to take precedence over eslint.config.ts' );
}

const standardExtensions = [ 'js', 'mjs', 'cjs', 'ts', 'mts', 'cts' ];
standardExtensions.forEach( ( extension, index ) => {
  const config = readConfig( process.argv[ 6 + index ] );
  if ( config.settings.homeboyFixture !== extension ) {
    throw new Error( `Expected eslint.config.${ extension } to execute` );
  }
  if ( config.rules.eqeqeq[ 0 ] !== 2 ) {
    throw new Error( `Expected eslint.config.${ extension } to compose provider defaults` );
  }
} );
JS

for extension in js ts tsx; do
    node -e '
      const config = require( process.argv[ 1 ] );
      if ( config.rules.eqeqeq[ 0 ] !== 2 ) {
        throw new Error( `Expected ${ process.argv[ 2 ] } to receive WordPress defaults` );
      }
    ' "${TMP_DIR}/default-${extension}.json" "$extension"
done

run_provider() {
    local component_path="$1"
    local output_file="$2"

    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$component_path" \
    HOMEBOY_COMPONENT_ID="eslint-provider-fixture" \
    HOMEBOY_COMPONENT_TEXT_DOMAIN="eslint-provider-fixture" \
        bash "$ESLINT_RUNNER" > "$output_file" 2>&1
}

run_provider \
    "${FIXTURES_DIR}/repository@feature" \
    "${TMP_DIR}/worktree-runner.out"
run_provider \
    "${FIXTURES_DIR}/default-only" \
    "${TMP_DIR}/default-runner.out"

for output_file in \
    "${TMP_DIR}/worktree-runner.out" \
    "${TMP_DIR}/default-runner.out"; do
    if ! grep -Fq "ESLint linting passed" "$output_file"; then
        echo "Expected provider runner to lint JSX successfully" >&2
        cat "$output_file" >&2
        exit 1
    fi
    if grep -Fq "File ignored because no matching configuration was supplied" "$output_file"; then
        echo "Expected discovered JSX to receive matching configuration" >&2
        cat "$output_file" >&2
        exit 1
    fi
done

echo "WordPress ESLint provider config smoke passed"
