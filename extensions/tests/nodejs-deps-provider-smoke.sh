#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

PROJECT_DIR="$TMP_DIR/project"
BIN_DIR="$TMP_DIR/bin"
mkdir -p "$PROJECT_DIR" "$BIN_DIR"

cat >"$PROJECT_DIR/package.json" <<'JSON'
{
  "name": "fixture-node-project",
  "dependencies": {
    "left-pad": "1.3.0"
  },
  "devDependencies": {
    "vitest": "1.0.0"
  }
}
JSON
cat >"$PROJECT_DIR/package-lock.json" <<'JSON'
{
  "name": "fixture-node-project",
  "lockfileVersion": 3,
  "packages": {}
}
JSON

cat >"$BIN_DIR/npm" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${HOMEBOY_FAKE_NPM_LOG:?}"
exit 0
SH
chmod +x "$BIN_DIR/npm"

export HOMEBOY_COMPONENT_PATH="$PROJECT_DIR"
export HOMEBOY_EXTENSION_PATH="$ROOT_DIR/nodejs"
export HOMEBOY_DEPENDENCY_ADAPTERS_PATH="$ROOT_DIR/dependency-adapters/examples"
export HOMEBOY_FAKE_NPM_LOG="$TMP_DIR/npm.log"
export PATH="$BIN_DIR:$PATH"

status_json="$("$ROOT_DIR/nodejs/scripts/deps/deps-runner.sh" status)"
STATUS_JSON="$status_json" node <<'NODE'
const status = JSON.parse(process.env.STATUS_JSON);
if (status.package_manager !== 'npm') throw new Error(`expected npm, got ${status.package_manager}`);
if (!status.dependency_identities.includes('fixture-node-project')) throw new Error('missing package identity');
if (!status.packages.some((pkg) => pkg.name === 'left-pad' && pkg.manifest_section === 'dependencies')) throw new Error('missing dependency package');
if (!status.packages.some((pkg) => pkg.name === 'vitest' && pkg.manifest_section === 'devDependencies')) throw new Error('missing dev dependency package');
NODE

command_json="$("$ROOT_DIR/nodejs/scripts/deps/deps-runner.sh" install-command)"
COMMAND_JSON="$command_json" node <<'NODE'
const plan = JSON.parse(process.env.COMMAND_JSON);
if (JSON.stringify(plan.command) !== JSON.stringify(['npm', 'ci'])) {
  throw new Error(`expected npm ci install plan, got ${JSON.stringify(plan.command)}`);
}
NODE

"$ROOT_DIR/nodejs/scripts/deps/deps-runner.sh" install >/dev/null
if [ "$(cat "$HOMEBOY_FAKE_NPM_LOG")" != "ci" ]; then
    echo "expected install to run npm ci" >&2
    exit 1
fi

rm "$PROJECT_DIR/package-lock.json"
status_json="$($ROOT_DIR/nodejs/scripts/deps/deps-runner.sh status)"
STATUS_JSON="$status_json" node <<'NODE'
const status = JSON.parse(process.env.STATUS_JSON);
if (!status.errors.some((error) => error.code === 'nodejs.lockfile_missing')) {
  throw new Error(`expected missing lockfile error, got ${JSON.stringify(status.errors)}`);
}
NODE

set +e
command_json="$($ROOT_DIR/nodejs/scripts/deps/deps-runner.sh install-command)"
command_status=$?
set -e
if [ "$command_status" -eq 0 ]; then
    echo "expected install-command to fail without a lockfile" >&2
    exit 1
fi
COMMAND_JSON="$command_json" node <<'NODE'
const plan = JSON.parse(process.env.COMMAND_JSON);
if (!Array.isArray(plan.command) || plan.command.length !== 0) {
  throw new Error(`expected empty command for unsupported install plan, got ${JSON.stringify(plan.command)}`);
}
if (!plan.errors.some((error) => error.code === 'nodejs.lockfile_missing')) {
  throw new Error(`expected missing lockfile error, got ${JSON.stringify(plan.errors)}`);
}
NODE

cat >"$PROJECT_DIR/package-lock.json" <<'JSON'
{
  "name": "fixture-node-project",
  "lockfileVersion": 3,
  "packages": {}
}
JSON
touch "$PROJECT_DIR/yarn.lock"
status_json="$($ROOT_DIR/nodejs/scripts/deps/deps-runner.sh status)"
STATUS_JSON="$status_json" node <<'NODE'
const status = JSON.parse(process.env.STATUS_JSON);
if (!status.errors.some((error) => error.code === 'nodejs.lockfile_ambiguous')) {
  throw new Error(`expected ambiguous lockfile error, got ${JSON.stringify(status.errors)}`);
}
NODE

echo "nodejs deps provider smoke passed"
