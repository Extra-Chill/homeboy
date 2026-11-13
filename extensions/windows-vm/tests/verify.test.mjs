import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { verifyRun } from '../scripts/verify.mjs';

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'windows-vm-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const nonce = 'fixture-unique-nonce', startedAt = '2026-11-12T00:00:00.000Z';
  const metadata = { nonce, requestStartedAt: startedAt, startedAt, outputCreatedAt: '2026-11-12T00:00:01.000Z' };
  fs.writeFileSync(`${directory}/output.txt`, `hello\nEXIT:0 NONCE:${nonce}\n`);
  fs.writeFileSync(`${directory}/artifact.txt`, 'artifact');
  const save = () => fs.writeFileSync(`${directory}/result.json`, JSON.stringify(metadata));
  save();
  return { directory, nonce, startedAt, metadata, save, artifacts: [{ remote: 'C:\\artifact.txt', local: `${directory}/artifact.txt` }] };
}
const fails = (f, type) => assert.throws(() => verifyRun(f), error => error.type === type);
test('fresh output, matching nonce and all artifacts succeed', t => {
  assert.equal(verifyRun(fixture(t)).exitCode, 0);
});
test('missing output fails', t => {
  const f = fixture(t); fs.unlinkSync(`${f.directory}/output.txt`); fails(f, 'missing_output');
});
test('missing trailer fails, including text after trailer', t => {
  const f = fixture(t);
  for (const text of ['NO OUTPUT', `EXIT:0 NONCE:${f.nonce}\nlate output`]) {
    fs.writeFileSync(`${f.directory}/output.txt`, text); fails(f, 'missing_trailer');
  }
});
test('wrong nonce fails even with a valid exit code', t => {
  const f = fixture(t); fs.writeFileSync(`${f.directory}/output.txt`, 'EXIT:0 NONCE:previous-run\n'); fails(f, 'nonce_mismatch');
});
test('stale creation timestamp and replayed request fail', t => {
  const f = fixture(t); f.metadata.outputCreatedAt = '2026-11-11T00:00:00Z'; f.save(); fails(f, 'stale_output');
  f.metadata.outputCreatedAt = f.startedAt; f.metadata.requestStartedAt = 'old'; f.save(); fails(f, 'stale_output');
});
test('missing artifact is never successful', t => {
  const f = fixture(t); fs.unlinkSync(f.artifacts[0].local); fails(f, 'missing_artifact');
});
test('nonzero command exit is typed', t => {
  const f = fixture(t); fs.writeFileSync(`${f.directory}/output.txt`, `EXIT:17 NONCE:${f.nonce}\n`); fails(f, 'command_failed');
});
test('absent, malformed and nonce-mismatched result fail', t => {
  const f = fixture(t); fs.unlinkSync(`${f.directory}/result.json`); fails(f, 'missing_result');
  fs.writeFileSync(`${f.directory}/result.json`, 'not json'); fails(f, 'invalid_result');
  f.metadata.nonce = 'old'; f.save(); fails(f, 'nonce_mismatch');
});
test('provider declares Windows x64 VM and its standalone lifecycle', () => {
  const manifest = JSON.parse(fs.readFileSync(new URL('../windows-vm.json', import.meta.url)));
  assert.deepEqual(manifest.environment_providers[0].environment, { platform: 'windows', arch: 'x86_64', execution_kind: 'vm' });
  assert.deepEqual(manifest.environment_providers[0].capabilities, ['prepare', 'reboot-cold', 'run', 'snapshot', 'reset', 'teardown']);
});
