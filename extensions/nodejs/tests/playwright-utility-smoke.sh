#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-playwright-utility.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

RUNTIME_DIR="$TMP_DIR/runtime"
NPM_LOG="$TMP_DIR/npm.log"
CLI_LOG="$TMP_DIR/cli.log"
FAKE_NPM="$TMP_DIR/npm"

cat > "$FAKE_NPM" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'npm ci\n' >> "$HOMEBOY_TEST_NPM_LOG"
prefix=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--prefix" ]; then prefix="$2"; shift 2; continue; fi
    shift
done
mkdir -p "$prefix/node_modules/playwright"
cat > "$prefix/node_modules/playwright/package.json" <<'PKG'
{"name":"playwright","version":"1.61.1","main":"index.js"}
PKG
cat > "$prefix/node_modules/playwright/index.js" <<'JS'
const path = require('node:path');
exports.chromium = {
  executablePath: () => path.join(__dirname, 'chromium-ready'),
  launch: async () => ({ close: async () => {} }),
};
JS
cat > "$prefix/node_modules/playwright/cli.js" <<'JS'
const fs = require('node:fs');
const path = require('node:path');
const browser = path.join(__dirname, 'chromium-ready');
fs.writeFileSync(browser, '#!/bin/sh\necho Chromium fake\n');
fs.chmodSync(browser, 0o700);
fs.appendFileSync(process.env.HOMEBOY_TEST_CLI_LOG, 'install chromium\n');
JS
EOF
chmod +x "$FAKE_NPM"

UTILITY="$ROOT_DIR/scripts/browser/playwright.sh"
HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" \
HOMEBOY_NODEJS_PLAYWRIGHT_NPM="$FAKE_NPM" \
HOMEBOY_TEST_NPM_LOG="$NPM_LOG" \
HOMEBOY_TEST_CLI_LOG="$CLI_LOG" \
    bash "$UTILITY" setup >/dev/null
HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" \
HOMEBOY_NODEJS_PLAYWRIGHT_NPM="$FAKE_NPM" \
HOMEBOY_TEST_NPM_LOG="$NPM_LOG" \
HOMEBOY_TEST_CLI_LOG="$CLI_LOG" \
    bash "$UTILITY" setup >/dev/null
[ "$(wc -l < "$NPM_LOG" | tr -d ' ')" = "1" ] || { echo 'setup reran npm ci' >&2; exit 1; }
[ "$(wc -l < "$CLI_LOG" | tr -d ' ')" = "1" ] || { echo 'setup reran Chromium installation' >&2; exit 1; }

STATUS="$(HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" bash "$UTILITY" status --json)"
node -e 'const status = JSON.parse(process.argv[1]); if (status.package.state !== "ready" || status.chromium.state !== "ready") process.exit(1)' "$STATUS"

for dialect in gnu uutils bsd; do
    FAKE_STAT="$TMP_DIR/stat-$dialect"
    STAT_LOG="$TMP_DIR/stat-$dialect.log"
    cat > "$FAKE_STAT" <<EOF
#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "\$*" >> "$STAT_LOG"
case "\$1" in
  --version)
    case "$dialect" in gnu) printf 'stat (GNU coreutils) 9.0\\n' ;; uutils) printf 'stat (uutils coreutils) 0.8.0\\n' ;; *) exit 1 ;; esac
    ;;
  -c)
    { [ "$dialect" = gnu ] || [ "$dialect" = uutils ]; } || exit 64
    case "\$2" in '%u') id -u ;; '%a') printf '700\\n' ;; *) exit 64 ;; esac
    ;;
  -f)
    if [ "$dialect" = gnu ]; then printf 'filesystem-data\\n'; exit 0; fi
    [ "$dialect" = bsd ] || exit 64
    case "\$2" in '%u') id -u ;; '%Lp') printf '700\\n' ;; *) exit 64 ;; esac
    ;;
  *) exit 64 ;;
esac
EOF
    chmod +x "$FAKE_STAT"
    DIALECT_STATUS="$(HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" HOMEBOY_NODEJS_PLAYWRIGHT_STAT="$FAKE_STAT" bash "$UTILITY" status --json)"
    node -e 'const status = JSON.parse(process.argv[1]); if (status.package.state !== "ready") process.exit(1)' "$DIALECT_STATUS"
    if [ "$dialect" = gnu ] || [ "$dialect" = uutils ]; then
        ! grep -q '^-f ' "$STAT_LOG" || { echo "$dialect stat used BSD -f probe" >&2; exit 1; }
    else
        grep -q '^-c ' "$STAT_LOG" || { echo 'BSD stat did not receive GNU compatibility probe' >&2; exit 1; }
        grep -q '^-f ' "$STAT_LOG" || { echo 'BSD stat did not receive BSD fallback probe' >&2; exit 1; }
    fi
done

PROJECT_DIR="$TMP_DIR/fresh-project"
mkdir -p "$PROJECT_DIR"
cat > "$PROJECT_DIR/extension-runtime.mjs" <<'EOF'
import { chromium } from 'playwright';
if (!chromium.executablePath().endsWith('chromium-ready')) throw new Error('extension Playwright was not resolved');
EOF
HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" bash "$UTILITY" execute "$PROJECT_DIR/extension-runtime.mjs"

cat > "$PROJECT_DIR/dynamic-runtime.mjs" <<'EOF'
const p = await import('playwright');
if (!p.chromium || typeof p.chromium.launch !== 'function') throw new Error('dynamic import did not expose chromium');
const browser = await p.chromium.launch();
await browser.close();
if (p.default?.chromium !== p.chromium) throw new Error('default export did not preserve the Playwright module');
EOF
HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" bash "$UTILITY" execute "$PROJECT_DIR/dynamic-runtime.mjs"

cat > "$PROJECT_DIR/action-runtime.mjs" <<'EOF'
import { chromium } from 'playwright';
if (!chromium.executablePath().endsWith('chromium-ready')) throw new Error('action did not use extension runtime');
if (JSON.stringify(process.argv.slice(2)) !== JSON.stringify(['--literal', 'a b', 'line one\nline two'])) throw new Error(`action args changed: ${JSON.stringify(process.argv)}`);
EOF

mkdir -p "$PROJECT_DIR/node_modules/playwright"
cat > "$PROJECT_DIR/node_modules/playwright/package.json" <<'EOF'
{"name":"playwright","version":"0.0.0-local","type":"module","exports":"./index.js"}
EOF
cat > "$PROJECT_DIR/node_modules/playwright/index.js" <<'EOF'
export const chromium = { executablePath: () => 'project-local' };
EOF
cat > "$PROJECT_DIR/project-local.mjs" <<'EOF'
import { chromium } from 'playwright';
if (!chromium.executablePath().endsWith('chromium-ready')) throw new Error('utility did not force its pinned Playwright runtime');
EOF
HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" bash "$UTILITY" execute "$PROJECT_DIR/project-local.mjs"

HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" \
HOMEBOY_SETTINGS_JSON="{\"selected\":[{\"module\":\"$PROJECT_DIR/action-runtime.mjs\",\"args\":[\"--literal\",\"a b\",\"line one\\nline two\"]}]}" \
    bash "$UTILITY" action-execute

for invalid_settings in \
    '{"selected":[]}' \
    "{\"selected\":[{\"module\":\"$PROJECT_DIR/action-runtime.mjs\"},{\"module\":\"$PROJECT_DIR/action-runtime.mjs\"}]}" \
    '{"selected":["not-an-object"]}' \
    '{"selected":[{"module":"","args":[]}]}' \
    '{"selected":[{"module":"module.mjs","args":[1]}]}'
do
    if HOMEBOY_SETTINGS_JSON="$invalid_settings" bash "$UTILITY" action-execute >/dev/null 2>&1; then
        echo "invalid selected-row action payload was accepted: $invalid_settings" >&2
        exit 1
    fi
done

MISSING_RUNTIME="$TMP_DIR/missing-runtime"
MISSING_STATUS="$(HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$MISSING_RUNTIME" bash "$UTILITY" status --json)"
node -e 'const status = JSON.parse(process.argv[1]); if (status.package.state !== "missing" || status.chromium.state !== "missing" || !status.setup_command) process.exit(1)' "$MISSING_STATUS"

mkdir -p "$MISSING_RUNTIME/package/node_modules/playwright"
printf '{"name":"playwright","version":"1.61.1"}\n' > "$MISSING_RUNTIME/package/node_modules/playwright/package.json"
PACKAGE_ONLY_STATUS="$(HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$MISSING_RUNTIME" bash "$UTILITY" status --json)"
node -e 'const status = JSON.parse(process.argv[1]); if (status.package.state !== "invalid" || status.chromium.state !== "missing") process.exit(1)' "$PACKAGE_ONLY_STATUS"

printf 'not executable\n' > "$RUNTIME_DIR/package/node_modules/playwright/chromium-ready"
chmod 600 "$RUNTIME_DIR/package/node_modules/playwright/chromium-ready"
INVALID_BROWSER_STATUS="$(HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" bash "$UTILITY" status --json)"
node -e 'const status = JSON.parse(process.argv[1]); if (status.chromium.state !== "invalid" || !status.chromium.reason) process.exit(1)' "$INVALID_BROWSER_STATUS"
HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" HOMEBOY_NODEJS_PLAYWRIGHT_NPM="$FAKE_NPM" HOMEBOY_TEST_NPM_LOG="$NPM_LOG" HOMEBOY_TEST_CLI_LOG="$CLI_LOG" bash "$UTILITY" setup >/dev/null

printf 'stale-runtime-identity\n' > "$RUNTIME_DIR/package/.homeboy-runtime-sha256"
HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" HOMEBOY_NODEJS_PLAYWRIGHT_NPM="$FAKE_NPM" HOMEBOY_TEST_NPM_LOG="$NPM_LOG" HOMEBOY_TEST_CLI_LOG="$CLI_LOG" bash "$UTILITY" setup >/dev/null
[ "$(wc -l < "$NPM_LOG" | tr -d ' ')" = "2" ] || { echo 'stale runtime identity did not rerun npm ci' >&2; exit 1; }

chmod 770 "$RUNTIME_DIR"
WRITABLE_STATUS="$(HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" bash "$UTILITY" status --json)"
node -e 'const status = JSON.parse(process.argv[1]); if (status.package.state !== "invalid") process.exit(1)' "$WRITABLE_STATUS"
chmod 700 "$RUNTIME_DIR"
ln -s "$RUNTIME_DIR/package/node_modules/playwright" "$RUNTIME_DIR/package/node_modules/playwright-link"
mv "$RUNTIME_DIR/package/node_modules/playwright" "$RUNTIME_DIR/package/node_modules/playwright-real"
mv "$RUNTIME_DIR/package/node_modules/playwright-link" "$RUNTIME_DIR/package/node_modules/playwright"
SYMLINK_STATUS="$(HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" bash "$UTILITY" status --json)"
node -e 'const status = JSON.parse(process.argv[1]); if (status.package.state !== "invalid") process.exit(1)' "$SYMLINK_STATUS"
rm "$RUNTIME_DIR/package/node_modules/playwright"
mv "$RUNTIME_DIR/package/node_modules/playwright-real" "$RUNTIME_DIR/package/node_modules/playwright"

cat > "$PROJECT_DIR/commonjs-runtime.cjs" <<'EOF'
const { chromium } = require('playwright');
if (!chromium.executablePath().endsWith('chromium-ready')) throw new Error('CommonJS did not use extension runtime');
EOF
HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" bash "$UTILITY" execute "$PROJECT_DIR/commonjs-runtime.cjs"

node <<'NODE'
const fs = require('node:fs');
const manifest = JSON.parse(fs.readFileSync('nodejs/nodejs.json'));
const action = manifest.actions.find(({ id }) => id === 'browser.playwright.execute');
if (!action || !action.command.endsWith(' action-execute') || action.payload.selected !== '{{selected}}') process.exit(1);
const docs = fs.readFileSync('nodejs/docs/browser-bench-helpers.md', 'utf8');
if (!docs.includes("browser.playwright.execute --data '[{")) process.exit(1);
NODE

node <<'NODE'
const fs = require('node:fs');
const expected = JSON.parse(fs.readFileSync('nodejs/tests/fixtures/playwright-1.61.1-root-exports.json', 'utf8')).sort();
const facade = fs.readFileSync('nodejs/scripts/browser/playwright-esm-facade.mjs', 'utf8');
const actual = [...facade.matchAll(/^export const ([A-Za-z_$][\w$]*) = playwright\./gm)].map((match) => match[1]).sort();
if (JSON.stringify(actual) !== JSON.stringify(expected)) {
  throw new Error(`Playwright 1.61.1 root facade exports drifted: ${JSON.stringify(actual)}`);
}
if (/export const (test|expect|defineConfig)\b/.test(facade)) {
  throw new Error('playwright/test exports must not be exposed by the root facade');
}
const loader = fs.readFileSync('nodejs/scripts/browser/playwright-loader.mjs', 'utf8');
if (!loader.includes("specifier.startsWith('playwright/')") || !loader.includes('nextResolve(specifier')) {
  throw new Error('Playwright subpath resolution must use Node package resolution.');
}
NODE

LINKED_ROOT="$TMP_DIR/linked-extension"
ln -s "$ROOT_DIR" "$LINKED_ROOT"
HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$RUNTIME_DIR" bash "$LINKED_ROOT/scripts/browser/playwright.sh" execute "$PROJECT_DIR/project-local.mjs"

echo "Node.js Playwright utility smoke passed."
