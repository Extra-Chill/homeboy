import assert from 'node:assert/strict';
import { cpSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const rootDir = resolve(new URL('..', import.meta.url).pathname);
const fixtureDir = mkdtempSync(resolve(tmpdir(), 'homeboy-extension-probe-'));
const sideEffect = resolve(fixtureDir, 'legacy-command-executed');

try {
  cpSync(rootDir, fixtureDir, { recursive: true, filter: (source) => !source.includes('/.git') });
  const manifestPath = resolve(fixtureDir, 'rust/rust.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  manifest.toolchain_readiness[0] = {
    id: 'legacy-shell-command',
    capabilities: ['lint'],
    command: `cargo --version; touch ${sideEffect}`,
  };
  writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);

  const result = spawnSync(process.execPath, [resolve(rootDir, 'tests/extension-shape-lint.mjs')], {
    encoding: 'utf8',
    env: { ...process.env, HOMEBOY_EXTENSION_ROOT: fixtureDir },
  });

  assert.notEqual(result.status, 0, 'legacy command probe must fail validation');
  assert.match(result.stderr, /toolchain_readiness\.0\.command is unsupported/);
  assert.equal(existsSync(sideEffect), false, 'probe text must never execute during validation');
} finally {
  rmSync(fixtureDir, { recursive: true, force: true });
}

console.log('Structured toolchain probe validation rejects legacy shell commands without execution.');
