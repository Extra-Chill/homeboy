import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const lint = fileURLToPath(new URL('./extension-shape-lint.mjs', import.meta.url));
const fixture = mkdtempSync(join(tmpdir(), 'homeboy-extension-scripts-'));

function check() {
  return spawnSync(process.execPath, [lint], {
    encoding: 'utf8',
    env: { ...process.env, HOMEBOY_EXTENSION_ROOT: fixture },
  });
}

try {
  cpSync(root, fixture, {
    recursive: true,
    filter: (source) => !['.git', 'node_modules', 'vendor', 'target'].includes(basename(source)),
  });

  const script = 'scripts/test/orphan-fixture.sh';
  const adapter = 'wordpress/wp-codebox-runtime/scripts/lib/wp-codebox-test-result-adapters.sh';
  writeFileSync(join(fixture, 'wordpress', script), '#!/usr/bin/env bash\n# orphan-fixture.sh\n');
  rmSync(join(fixture, adapter));
  rmSync(join(fixture, 'swift/scripts/setup.sh'));

  const broken = check();
  assert.equal(broken.status, 1, broken.stderr);
  assert.match(broken.stderr, /wordpress: undeclared script scripts\/test\/orphan-fixture\.sh/);
  assert.match(broken.stderr, /wordpress\/wp-codebox-runtime: files references missing file scripts\/lib\/wp-codebox-test-result-adapters\.sh/);
  assert.match(broken.stderr, /swift: executable\.runtime\.setup_command references missing file scripts\/setup\.sh/);

  cpSync(join(root, adapter), join(fixture, adapter));
  cpSync(join(root, 'swift/scripts/setup.sh'), join(fixture, 'swift/scripts/setup.sh'));
  const metadataPath = join(fixture, 'wordpress/homeboy.json');
  const metadata = JSON.parse(readFileSync(metadataPath, 'utf8'));
  metadata.self_checks.test.push(`bash ${script}`);
  writeFileSync(metadataPath, JSON.stringify(metadata));

  const repaired = check();
  assert.equal(repaired.status, 0, repaired.stderr);
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
