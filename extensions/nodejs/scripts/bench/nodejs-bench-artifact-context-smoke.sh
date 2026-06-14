#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-bench-artifact-context.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

HOMEBOY_BENCH_RESULTS_FILE="$TMP_DIR/results/bench-results.json" \
HOMEBOY_BENCH_ARTIFACTS_DIR="$TMP_DIR/artifacts" \
HELPER_UNDER_TEST="$SCRIPT_DIR/lib/artifact-context.mjs" \
REDACTION_UNDER_TEST="$SCRIPT_DIR/lib/redaction.mjs" \
node --input-type=module - <<'EOF'
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const { createBenchArtifactContext } = await import(process.env.HELPER_UNDER_TEST);
const { sanitizeArtifactFile, sanitizeArtifactValue, redactText, sanitizeUrl } = await import(process.env.REDACTION_UNDER_TEST);

const sharedState = {};
const context = createBenchArtifactContext({
  id: 'Smoke Scenario',
  sharedState,
  runId: 'smoke-run-001',
});

if (context.runId !== 'smoke-run-001') throw new Error('run id override not preserved');
if (context.rootDir !== resolve(process.env.HOMEBOY_BENCH_ARTIFACTS_DIR)) throw new Error('artifact root did not use env dir');
if (context.artifactDir !== resolve(process.env.HOMEBOY_BENCH_ARTIFACTS_DIR, 'smoke-run-001')) throw new Error('artifact dir not scoped by run id');
if (context.artifactPath('raw result', { kind: 'json' }) !== join(context.artifactDir, 'result-raw-result.json')) throw new Error('artifact path was not sanitized');

const descriptor = await context.writeJson('raw_result', {
  ok: true,
  elapsed_ms: Number.POSITIVE_INFINITY,
  url: 'https://example.test/callback?token=abc123&keep=visible',
  headers: {
    authorization: 'Bearer abc123',
    'x-request-id': 'diag-1',
  },
  body: {
    password: 'secret',
    nested: { api_key: 'key-123', useful: 'kept' },
  },
}, { label: 'Raw result' });

if (descriptor.kind !== 'json') throw new Error('writeJson descriptor kind missing');
if (descriptor.label !== 'Raw result') throw new Error('writeJson descriptor label missing');
if (context.artifacts.raw_result.path !== descriptor.path) throw new Error('descriptor not recorded on context');

const viewerDescriptor = context.addArtifact('viewer_input', join(context.artifactDir, 'viewer-input.json'), {
  kind: 'json',
  label: 'Viewer input',
  url: 'https://artifacts.example.test/runs/smoke/viewer-input.json',
  viewer: { kind: 'opaque-viewer', url: 'https://viewer.example.test/runs/smoke' },
});
if (viewerDescriptor.url !== 'https://artifacts.example.test/runs/smoke/viewer-input.json') throw new Error('artifact URL not preserved');
if (viewerDescriptor.viewer.kind !== 'opaque-viewer') throw new Error('viewer metadata not preserved');

const written = JSON.parse(await readFile(descriptor.path, 'utf8'));
if (written.elapsed_ms !== null) throw new Error('non-finite number was not normalized');
if (written.url.includes('abc123')) throw new Error('URL token was not redacted in JSON artifact');
if (written.headers.authorization !== '[REDACTED]') throw new Error('authorization header was not redacted');
if (written.headers['x-request-id'] !== 'diag-1') throw new Error('diagnostic header was destroyed');
if (written.body.password !== '[REDACTED]') throw new Error('password field was not redacted');
if (written.body.nested.useful !== 'kept') throw new Error('useful diagnostic field was destroyed');

const secondContext = createBenchArtifactContext({ id: 'Smoke Scenario', sharedState, runId: 'ignored-run' });
if (secondContext.runId !== context.runId) throw new Error('sharedState did not preserve run id');

const sanitized = sanitizeArtifactValue({
  request: {
    url: 'https://example.test/?access_token=abc&search=term',
    headers: { cookie: 'sid=abc', accept: 'application/json' },
  },
});
if (sanitized.request.url.includes('abc')) throw new Error('access_token query value leaked');
if (sanitized.request.headers.cookie !== '[REDACTED]') throw new Error('cookie header leaked');
if (sanitized.request.headers.accept !== 'application/json') throw new Error('safe header was redacted');

const redactedUrl = sanitizeUrl('https://user:pass@example.test/path?nonce=abc&debug=1');
if (redactedUrl.includes('abc') || redactedUrl.includes('pass')) throw new Error('URL credentials/query secrets leaked');
if (!redactedUrl.includes('debug=1')) throw new Error('safe query diagnostics were removed');

const text = redactText('Authorization: Bearer abc123 https://example.test/?api_key=secret&phase=load session_token=abc');
if (text.includes('abc123') || text.includes('secret') || text.includes('session_token=abc')) throw new Error('text redaction missed common secrets');
if (!text.includes('phase=load')) throw new Error('text redaction removed useful query context');

const textFile = join(process.env.HOMEBOY_BENCH_ARTIFACTS_DIR, 'artifact-text.txt');
await import('node:fs/promises').then(({ writeFile }) => writeFile(textFile, 'password=hunter2 keep=visible'));
await sanitizeArtifactFile(textFile);
const sanitizedText = await readFile(textFile, 'utf8');
if (sanitizedText.includes('hunter2') || !sanitizedText.includes('keep=visible')) throw new Error('sanitizeArtifactFile text mode failed');
EOF

echo "Node.js bench artifact context smoke passed."
