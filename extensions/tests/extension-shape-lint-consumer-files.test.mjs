import assert from 'node:assert/strict';
import { cpSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

// Homeboy adds only homeboy.json to a consumer repository
// (Extra-Chill/homeboy#16143). The shape lint must reject an extension that
// reads or documents any other Homeboy-named consumer-repo file.

const rootDir = resolve(new URL('..', import.meta.url).pathname);
const lint = resolve(rootDir, 'tests/extension-shape-lint.mjs');

function lintFixture(mutate) {
  const fixtureDir = mkdtempSync(resolve(tmpdir(), 'homeboy-extension-consumer-files-'));
  try {
    cpSync(rootDir, fixtureDir, {
      recursive: true,
      filter: (source) => !source.includes('/.git') && !source.includes('/node_modules'),
    });
    mutate(fixtureDir);
    return spawnSync(process.execPath, [lint], {
      encoding: 'utf8',
      env: { ...process.env, HOMEBOY_EXTENSION_ROOT: fixtureDir },
    });
  } finally {
    rmSync(fixtureDir, { recursive: true, force: true });
  }
}

const clean = lintFixture(() => {});
assert.equal(clean.status, 0, `clean tree must pass:\n${clean.stderr}`);

const reader = lintFixture((dir) => {
  writeFileSync(
    resolve(dir, 'wordpress/scripts/test/consumer-reader.sh'),
    'manifest="${PLUGIN_PATH}/homeboy-test-manifest.json"\n',
  );
});
assert.notEqual(reader.status, 0, 'a consumer-repo file reader must fail the lint');
assert.match(reader.stderr, /consumer-reader\.sh: reads or documents consumer-repo file homeboy-test-manifest\.json/);

const dotDir = lintFixture((dir) => {
  writeFileSync(resolve(dir, 'rust/NOTES.md'), 'Declare settings in `.homeboy/rust-settings.json`.\n');
});
assert.notEqual(dotDir.status, 0, 'documenting a .homeboy/ consumer file must fail the lint');
assert.match(dotDir.stderr, /rust\/NOTES\.md: reads or documents consumer-repo file \.homeboy\/rust-settings\.json/);

const retired = lintFixture((dir) => {
  writeFileSync(resolve(dir, 'wordpress/docs/RETIRED.md'), 'Schema `homeboy/test-manifest/v1`.\n');
});
assert.notEqual(retired.status, 0, 'a retired consumer declaration contract must fail the lint');
assert.match(retired.stderr, /RETIRED\.md: references retired consumer-repo declaration homeboy\/test-manifest\/v1/);

console.log('Extension shape lint rejects consumer-repo files other than homeboy.json.');
