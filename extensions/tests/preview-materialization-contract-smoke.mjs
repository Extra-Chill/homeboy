#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
	PREVIEW_MATERIALIZATION_EVIDENCE_SCHEMA,
	PREVIEW_MATERIALIZATION_REQUEST_SCHEMA,
	materializePreview,
	normalizePreviewMaterializationRequest,
} = require(path.join(rootDir, 'runtime-agent-ci', 'provider-adapters.js'));

const request = normalizePreviewMaterializationRequest({
	id: 'preview-1',
	adapter: 'fake-preview-runtime',
	domainInput: { kind: 'artifact', artifact_id: 'site-packet-1' },
	lease: { ttl_seconds: 300 },
	boot: { mode: 'review' },
});

assert.equal(request.schema, PREVIEW_MATERIALIZATION_REQUEST_SCHEMA);
assert.deepEqual(request.target, { kind: 'artifact', artifact_id: 'site-packet-1' });

const fakeAdapter = {
	id: 'fake-preview-runtime',
	async materializePreview(previewRequest) {
		assert.equal(previewRequest.schema, PREVIEW_MATERIALIZATION_REQUEST_SCHEMA);
		return {
			url: 'https://preview.example.test/session/preview-1/',
			lease: { id: 'lease-1', ttl_seconds: 300 },
			boot: { mode: 'review' },
			status: { state: 'ready' },
		};
	},
};

const evidence = await materializePreview(fakeAdapter, request);
assert.equal(evidence.schema, PREVIEW_MATERIALIZATION_EVIDENCE_SCHEMA);
assert.equal(evidence.adapter, 'fake-preview-runtime');
assert.equal(evidence.url, 'https://preview.example.test/session/preview-1/');
assert.deepEqual(evidence.refs, [{ kind: 'preview', uri: 'https://preview.example.test/session/preview-1/', label: 'Preview' }]);
assert.deepEqual(evidence.lease, { id: 'lease-1', ttl_seconds: 300 });
assert.deepEqual(evidence.boot, { mode: 'review' });
assert.deepEqual(evidence.status, { state: 'ready' });

console.log('preview materialization contract smoke passed');
