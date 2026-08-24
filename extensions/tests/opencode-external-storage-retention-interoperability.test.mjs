import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const extensionRoot = new URL('..', import.meta.url).pathname;
const manifest = JSON.parse(await (await import('node:fs/promises')).readFile(join(extensionRoot, 'homeboy.json'), 'utf8'));
const providers = manifest.external_storage_retention.providers;
assert.equal(providers.length, 1);
const [provider] = providers;
assert.deepEqual(provider, {
  id: 'opencode.external-storage-retention',
  command: ['node', '{{extension_path}}/agent-runtimes/opencode/scripts/agent/homeboy-opencode-external-storage-retention.cjs'],
  timeout_seconds: 30,
});
const root = mkdtempSync(join(tmpdir(), 'opencode-retention-contract-'));
const data = join(root, 'data');
mkdirSync(data);
const command = join(root, 'opencode.cjs');
writeFileSync(command, `#!/usr/bin/env node\nif (process.argv.slice(2).join(' ') === 'debug paths') process.stdout.write('data  ${data}\\n');\nelse if (process.argv.slice(2).join(' ') === 'db path') process.stdout.write('${join(data, 'opencode.db')}\\n');\nelse if (process.argv.includes('list')) process.stdout.write('[]');\n`);
writeFileSync(join(data, 'opencode.db'), 'db');
const config = join(root, 'config.json');
writeFileSync(config, JSON.stringify({ command, data_roots: [data] }));
const [program, template] = provider.command;
const result = spawnSync(program, [template.replace('{{extension_path}}', extensionRoot)], {
  input: JSON.stringify({ schema: 'homeboy/external-storage-retention/v1', operation: 'inventory' }),
  encoding: 'utf8',
  env: { ...process.env, HOMEBOY_OPENCODE_RETENTION_CONFIG: config, XDG_STATE_HOME: join(root, 'state') },
});
assert.equal(result.status, 0, result.stderr);
const inventory = JSON.parse(result.stdout);
assert.equal(inventory.schema, 'homeboy/external-storage-retention/v1');
assert.equal(inventory.provider_id, provider.id);
