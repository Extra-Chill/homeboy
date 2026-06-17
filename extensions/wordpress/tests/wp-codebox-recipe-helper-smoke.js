'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {
  parseWpCodeboxJson,
  recipeEventName,
  runWpCodeboxRecipe,
  wpCodeboxBin,
  wpCodeboxCommand,
} = require('../lib/wp-codebox-recipe-helper');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-recipe-helper-'));
const fixtureBin = path.join(root, 'fixture-wp-codebox.cjs');
const capturePath = path.join(root, 'capture.json');
const recipeFile = path.join(root, 'recipe.json');
const artifactsDir = path.join(root, 'artifacts');
const outputFile = path.join(root, 'output', 'wp-codebox-output.json');

fs.writeFileSync(recipeFile, JSON.stringify({ schema: 'wp-codebox/workspace-recipe/v1' }));
fs.writeFileSync(fixtureBin, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
fs.writeFileSync(process.env.FIXTURE_CAPTURE_PATH, JSON.stringify({ argv: process.argv.slice(2) }, null, 2));
if (process.env.FIXTURE_FAIL) {
  process.stdout.write(JSON.stringify({ ok: false, failure: true }));
  process.exit(7);
}
process.stdout.write(JSON.stringify({ ok: true, artifacts: { directory: process.argv[process.argv.indexOf('--artifacts') + 1] } }));
`);
fs.chmodSync(fixtureBin, 0o755);

(async () => {
  assert.equal(wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_BIN: fixtureBin } }), fixtureBin);
  assert.equal(wpCodeboxBin({ env: {} }), 'wp-codebox');
  assert.deepEqual(wpCodeboxCommand('/tmp/wp-codebox.cjs'), { command: process.execPath, args: ['/tmp/wp-codebox.cjs'] });
  assert.deepEqual(wpCodeboxCommand('wp-codebox'), { command: 'wp-codebox', args: [] });
  assert.equal(recipeEventName('start'), 'recipe.start');
  assert.equal(recipeEventName('start', { eventPrefix: 'sandbox.recipe' }), 'sandbox.recipe.start');
  assert.deepEqual(parseWpCodeboxJson(' {"ok": true}\n'), { ok: true });

  const events = [];
  const result = await runWpCodeboxRecipe({
    recipeFile,
    artifactsDir,
    outputFile,
    wpCodeboxBin: fixtureBin,
    recipeRunArgs: ['--fixture'],
    env: { ...process.env, FIXTURE_CAPTURE_PATH: capturePath },
    event: (source, name, data) => events.push({ source, name, data }),
  });

  assert.deepEqual(result.json, { ok: true, artifacts: { directory: artifactsDir } });
  assert.deepEqual(JSON.parse(fs.readFileSync(outputFile, 'utf8')), result.json);
  assert.deepEqual(events.map((entry) => [entry.source, entry.name]), [
    ['wp_codebox', 'recipe.start'],
    ['wp_codebox', 'recipe.done'],
  ]);
  const capture = JSON.parse(fs.readFileSync(capturePath, 'utf8'));
  assert.deepEqual(capture.argv, ['recipe-run', '--recipe', recipeFile, '--artifacts', artifactsDir, '--fixture', '--json']);

  await assert.rejects(
    runWpCodeboxRecipe({
      recipeFile,
      artifactsDir,
      outputFile,
      wpCodeboxBin: fixtureBin,
      env: { ...process.env, FIXTURE_CAPTURE_PATH: capturePath, FIXTURE_FAIL: '1' },
      event: (source, name, data) => events.push({ source, name, data }),
    }),
    /Command failed/
  );
  assert.equal(events.at(-1).name, 'recipe.failed');
  assert.deepEqual(JSON.parse(fs.readFileSync(outputFile, 'utf8')), { ok: false, failure: true });

  console.log('wp-codebox recipe helper smoke passed');
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
