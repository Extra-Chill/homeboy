import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const writer = fileURLToPath(new URL('./test-failure-record.mjs', import.meta.url));
function publish(stdout) {
    const root = mkdtempSync(join(tmpdir(), 'failure-record-'));
    try {
        const input = join(root, 'input');
        writeFileSync(input, `${stdout}\0`, { mode: 0o600 });
        return JSON.parse(execFileSync(process.execPath, [
            writer, 'rust', 'test-id', '', '', '', 'assertion failed', 'test_failure', input,
        ], { encoding: 'utf8' })).stdout_excerpt;
    } finally {
        rmSync(root, { recursive: true, force: true });
    }
}

test('published quoted secrets include whitespace, punctuation and escaped quotes', () => {
    for (const raw of [
        JSON.stringify({ password: 'alpha beta; gamma,delta' }),
        JSON.stringify({ refresh_token: 'escaped "quoted" secret \\' }),
        JSON.stringify(JSON.stringify({ access_token: 'alpha beta; "quoted" \\' })),
        "password='alpha beta; gamma,delta'",
    ]) {
        const result = publish(raw);
        for (const secret of ['alpha', 'beta', 'gamma', 'delta', 'escaped', 'quoted']) {
            assert.ok(!result.includes(secret), result);
        }
        assert.ok(result.includes('[REDACTED]'), result);
    }
});

test('a long secret is redacted before its key could fall outside the retained tail', () => {
    const raw = `password=${'sensitive-fragment-'.repeat(1000)}`;
    assert.equal(publish(raw), 'password=[REDACTED]');
});

test('expanded replacements and multibyte tails remain within the published byte bound', () => {
    const result = publish(`${'password=x '.repeat(1000)}${'\u2603'.repeat(2000)}`);
    assert.ok(Buffer.byteLength(result, 'utf8') <= 4096);
    assert.ok(!result.includes('\ufffd'));
    assert.match(result, /^\[truncated \d+ UTF-8 bytes; retained tail\]/);
});
