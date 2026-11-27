import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { createPhpunitSuiteResults, readPhpunitSuiteResults } from '../scripts/lib/phpunit-suite-results.mjs';

const directory = await mkdtemp(path.join(os.tmpdir(), 'phpunit-suite-results-'));
try {
  const destination = path.join(directory, 'results.json');
  const record = createPhpunitSuiteResults(destination);
  await record({ source: 'current-runtime', total: 3, passed: 2, failed: 1, skipped: 0, partial: 'sandbox' });
  assert.deepEqual(await readPhpunitSuiteResults(destination), {
    source: 'current-runtime', total: 3, passed: 2, failed: 1, skipped: 0, partial: 'sandbox',
  });
  // A host suite must neither erase runtime metadata nor replace its counts.
  await record({ total: 2, passed: 1, failed: 0, skipped: 1 });
  await record({ source: 'current-runtime', total: 1, passed: 1, failed: 0, skipped: 0, partial: 'sandbox' });
  assert.deepEqual(await readPhpunitSuiteResults(destination), {
    source: 'current-runtime', total: 6, passed: 4, failed: 1, skipped: 1, partial: 'sandbox',
  });
  console.log('PHPUnit suite results smoke passed');
} finally {
  await rm(directory, { recursive: true, force: true });
}
