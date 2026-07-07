#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

PROJECT_DIR="$TMP_DIR/project"
BIN_DIR="$TMP_DIR/bin"
mkdir -p "$PROJECT_DIR" "$BIN_DIR"

cat >"$PROJECT_DIR/composer.json" <<'JSON'
{
  "name": "extra-chill/fixture-wordpress-component",
  "require": {
    "php": ">=8.2",
    "composer/installers": "^2.3"
  },
  "require-dev": {
    "phpunit/phpunit": "^11.0"
  }
}
JSON
cat >"$PROJECT_DIR/package.json" <<'JSON'
{
  "name": "fixture-wordpress-component",
  "dependencies": {
    "@wordpress/scripts": "^30.0.0"
  },
  "devDependencies": {
    "eslint": "^9.0.0"
  }
}
JSON
cat >"$PROJECT_DIR/package-lock.json" <<'JSON'
{
  "name": "fixture-wordpress-component",
  "lockfileVersion": 3,
  "packages": {}
}
JSON

cat >"$BIN_DIR/composer" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${HOMEBOY_FAKE_COMPOSER_LOG:?}"
exit 0
SH
cat >"$BIN_DIR/npm" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${HOMEBOY_FAKE_NPM_LOG:?}"
exit 0
SH
chmod +x "$BIN_DIR/composer" "$BIN_DIR/npm"

export HOMEBOY_COMPONENT_PATH="$PROJECT_DIR"
export HOMEBOY_FAKE_COMPOSER_LOG="$TMP_DIR/composer.log"
export HOMEBOY_FAKE_NPM_LOG="$TMP_DIR/npm.log"
export PATH="$BIN_DIR:$PATH"

status_json="$("$ROOT_DIR/scripts/deps/deps-runner.sh" status)"
STATUS_JSON="$status_json" node <<'NODE'
const status = JSON.parse(process.env.STATUS_JSON);
if (status.package_manager !== 'wordpress') throw new Error(`expected wordpress provider, got ${status.package_manager}`);
if (!status.dependency_identities.includes('extra-chill/fixture-wordpress-component')) throw new Error('missing composer identity');
if (!status.dependency_identities.includes('fixture-wordpress-component')) throw new Error('missing package identity');
if (!status.packages.some((pkg) => pkg.name === 'composer/installers' && pkg.manifest_section === 'require')) throw new Error('missing composer dependency');
if (!status.packages.some((pkg) => pkg.name === '@wordpress/scripts' && pkg.manifest_section === 'dependencies')) throw new Error('missing npm dependency');
NODE

command_json="$("$ROOT_DIR/scripts/deps/deps-runner.sh" install-command)"
COMMAND_JSON="$command_json" ROOT_DIR="$ROOT_DIR" node <<'NODE'
const plan = JSON.parse(process.env.COMMAND_JSON);
const expected = ['bash', `${process.env.ROOT_DIR}/scripts/deps/deps-runner.sh`, 'install'];
if (JSON.stringify(plan.command) !== JSON.stringify(expected)) {
  throw new Error(`expected ${JSON.stringify(expected)}, got ${JSON.stringify(plan.command)}`);
}
NODE

"$ROOT_DIR/scripts/deps/deps-runner.sh" install >/dev/null
if [ "$(cat "$HOMEBOY_FAKE_COMPOSER_LOG")" != "install --no-interaction --prefer-dist" ]; then
    echo "expected composer install command" >&2
    exit 1
fi
if [ "$(cat "$HOMEBOY_FAKE_NPM_LOG")" != "ci" ]; then
    echo "expected npm ci command" >&2
    exit 1
fi

node <<'NODE'
const fs = require('fs');
const manifest = JSON.parse(fs.readFileSync('wordpress.json', 'utf8'));
if (manifest.deps?.extension_script !== 'scripts/deps/deps-runner.sh') {
  throw new Error('wordpress manifest does not declare deps.extension_script');
}
NODE

echo "wordpress deps provider smoke passed"
