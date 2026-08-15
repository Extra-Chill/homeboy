import { strict as assert } from 'node:assert';

import {
  WP_CODEBOX_TIMEOUT_DIAGNOSTICS_MAX_BYTES,
  WP_CODEBOX_TIMEOUT_DIAGNOSTICS_SCHEMA,
  createTimeoutLineRedactor,
  wpCodeboxTimeoutDiagnostics,
} from '../scripts/lib/wp-codebox-timeout-diagnostics.mjs';

// This mirrors a JSON-serialized Buffer that was retained in a timed-out
// recipe-run response: enough numeric entries to exceed one megabyte without
// putting a giant fixture file in the repository.
const byteMap = Object.fromEntries(Array.from({ length: 131072 }, (_, index) => [index, index % 256]));
const payload = {
  success: false,
  executions: [{
    command: 'wordpress.run-php api_token=fixture-secret-value',
    status: 'running',
    stdout: byteMap,
    stderr: 'provider token=fixture-secret-value timed out after waiting for WordPress',
  }],
};

assert.ok(Buffer.byteLength(JSON.stringify(payload)) > 1024 * 1024);

const diagnostic = wpCodeboxTimeoutDiagnostics({
  phase: 'wp-codebox-recipe-run',
  elapsedSeconds: 1500,
  budgetSeconds: 1500,
  selected: ['tests/wordpress-timeout-smoke.php'],
  termination: { result: 'timeout', signal: 'SIGTERM' },
  artifacts: ['artifact:///tmp/run/recipe-run.json', 'artifact:///tmp/run/wp-codebox.stderr'],
  payload,
  stderr: 'provider token=fixture-secret-value timed out after waiting for WordPress',
  secretValues: ['fixture-secret-value'],
});

const serialized = JSON.stringify(diagnostic);
assert.equal(diagnostic.schema, WP_CODEBOX_TIMEOUT_DIAGNOSTICS_SCHEMA);
assert.equal(diagnostic.phase, 'wp-codebox-recipe-run');
assert.deepEqual(diagnostic.selected, { count: 1, items: ['tests/wordpress-timeout-smoke.php'] });
assert.deepEqual(diagnostic.execution, {
  count: 1,
  count_complete: true,
  completed_count: 0,
  last: {
    index: 0,
    command: 'wordpress.run-php api_token=[REDACTED]',
    status: 'running',
    stdout: { kind: 'byte_map_omitted' },
    stderr: { kind: 'text', bytes: Buffer.byteLength(payload.executions[0].stderr) },
  },
});
assert.deepEqual(diagnostic.termination, { result: 'timeout', signal: 'SIGTERM' });
assert.deepEqual(diagnostic.artifact_refs, ['artifact:///tmp/run/recipe-run.json', 'artifact:///tmp/run/wp-codebox.stderr']);
assert.match(diagnostic.excerpts, /token=\[REDACTED\]/);
assert.doesNotMatch(serialized, /fixture-secret-value|"0":0|"131071"|byteMap/);
assert.ok(Buffer.byteLength(serialized) <= WP_CODEBOX_TIMEOUT_DIAGNOSTICS_MAX_BYTES);

const redactionDiagnostic = wpCodeboxTimeoutDiagnostics({
  stderr: [
    'Authorization: Bearer bearer-secret',
    'Proxy-Authorization: Basic basic-secret',
    'Cookie: session=cookie-secret; theme=dark',
    'Set-Cookie: session=cookie-secret',
    '  Path=/; HttpOnly',
    'request=https://user:url-secret@example.test/private',
    'credential: multiline-secret',
    '  continuation-value',
    'environment=environment-secret',
  ].join('\n'),
  secretValues: ['bearer-secret', 'basic-secret', 'cookie-secret', 'url-secret', 'multiline-secret\ncontinuation-value', 'environment-secret'],
});
const redacted = JSON.stringify(redactionDiagnostic);
assert.doesNotMatch(redacted, /bearer-secret|basic-secret|cookie-secret|url-secret|multiline-secret|continuation-value|environment-secret/);
assert.match(redacted, /Authorization:\[REDACTED\]/);
assert.match(redacted, /Cookie:\[REDACTED\]/);
assert.match(redacted, /https:\/\/\[REDACTED\]@example\.test/);

const priorEnvironmentSecret = process.env.FIXTURE_SESSION_SECRET;
process.env.FIXTURE_SESSION_SECRET = 'environment-derived-secret';
const environmentDiagnostic = wpCodeboxTimeoutDiagnostics({ stderr: 'session=environment-derived-secret' });
assert.doesNotMatch(JSON.stringify(environmentDiagnostic), /environment-derived-secret/);
if (priorEnvironmentSecret === undefined) {
  delete process.env.FIXTURE_SESSION_SECRET;
} else {
  process.env.FIXTURE_SESSION_SECRET = priorEnvironmentSecret;
}

const spanningCredential = `Bearer ${'s'.repeat(9000)}`;
const streamRedactor = createTimeoutLineRedactor();
assert.equal(streamRedactor.write(`Authorization: ${spanningCredential.slice(0, 4096)}`), '');
assert.equal(streamRedactor.write(`${spanningCredential.slice(4096)}\n`), 'Authorization:[REDACTED]\n');
assert.doesNotMatch(streamRedactor.end(), /s/);
const overlongRedactor = createTimeoutLineRedactor();
assert.equal(overlongRedactor.write(`Cookie: session=${'x'.repeat(128 * 1024)}`), '');
assert.equal(overlongRedactor.end(), '[REDACTED OVERLONG LINE]\n');

console.log('WP Codebox timeout diagnostics smoke passed.');
