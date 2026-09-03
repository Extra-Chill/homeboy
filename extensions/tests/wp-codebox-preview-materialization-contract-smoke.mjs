#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(rootDir, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const {
	materializePreview,
} = require(path.join(rootDir, 'runtime-agent-ci', 'provider-adapters.js'));
const {
	WP_CODEBOX_BROWSER_CONTAINED_SITE_OPEN_SCHEMA,
	WP_CODEBOX_BROWSER_CONTAINED_SITE_STATUS_SCHEMA,
	WP_CODEBOX_BROWSER_PREVIEW_BOOT_CONFIG_SCHEMA,
	WP_CODEBOX_PREVIEW_LEASE_SCHEMA,
	codeboxPreviewEvidenceFromContainedSiteResult,
	codeboxPreviewMaterializationAdapter,
	codeboxPreviewOpenRequest,
} = require(path.join(rootDir, 'wordpress', 'lib', 'wp-codebox-preview-materialization-contract.js'));

const genericRequest = {
	id: 'codebox-preview-1',
	target: { kind: 'site-artifact', artifact_id: 'site-packet-1' },
	routes: { home: '/' },
	lease: { ttl_seconds: 600 },
	boot: { php_version: '8.3' },
	metadata: { source: 'test' },
};

const openRequest = codeboxPreviewOpenRequest(genericRequest);
assert.equal(openRequest.schema, WP_CODEBOX_BROWSER_CONTAINED_SITE_OPEN_SCHEMA);
assert.equal(openRequest.lease.schema, WP_CODEBOX_PREVIEW_LEASE_SCHEMA);
assert.equal(openRequest.boot.schema, WP_CODEBOX_BROWSER_PREVIEW_BOOT_CONFIG_SCHEMA);
assert.deepEqual(openRequest.target, { kind: 'site-artifact', artifact_id: 'site-packet-1' });

const adapter = codeboxPreviewMaterializationAdapter({
	async openContainedSite(request) {
		assert.equal(request.schema, WP_CODEBOX_BROWSER_CONTAINED_SITE_OPEN_SCHEMA);
		assert.deepEqual(request.target, genericRequest.target);
		return {
			preview_url: 'https://codebox-preview.example.test/site/',
			lease: { schema: WP_CODEBOX_PREVIEW_LEASE_SCHEMA, id: 'lease-codebox-1', ttl_seconds: 600 },
			boot: { schema: WP_CODEBOX_BROWSER_PREVIEW_BOOT_CONFIG_SCHEMA, php_version: '8.3' },
			status: { schema: WP_CODEBOX_BROWSER_CONTAINED_SITE_STATUS_SCHEMA, state: 'ready' },
		};
	},
});

const evidence = await materializePreview(adapter, genericRequest);
assert.equal(evidence.adapter, 'wp-codebox');
assert.equal(evidence.url, 'https://codebox-preview.example.test/site/');
assert.deepEqual(evidence.refs, [{ kind: 'codebox-preview', uri: 'https://codebox-preview.example.test/site/', label: 'WP Codebox preview' }]);
assert.equal(evidence.lease.schema, WP_CODEBOX_PREVIEW_LEASE_SCHEMA);
assert.equal(evidence.boot.schema, WP_CODEBOX_BROWSER_PREVIEW_BOOT_CONFIG_SCHEMA);
assert.equal(evidence.status.schema, WP_CODEBOX_BROWSER_CONTAINED_SITE_STATUS_SCHEMA);
assert.equal(evidence.metadata.codebox_open_schema, WP_CODEBOX_BROWSER_CONTAINED_SITE_OPEN_SCHEMA);

assert.equal(
	codeboxPreviewEvidenceFromContainedSiteResult({ contained_site: { url: 'https://contained.example.test', status: { state: 'booting' } } }).url,
	'https://contained.example.test'
);

const boundaryTerms = /wp-site-generator|WPSG|site-generator|site generator|PLAYGROUND_PREVIEW|PLAYGROUND_URL/i;
for (const relativePath of [
	'runtime-agent-ci/lib/preview-materialization.js',
	'wordpress/lib/wp-codebox-preview-materialization-contract.js',
]) {
	assert.equal(boundaryTerms.test(fs.readFileSync(path.join(rootDir, relativePath), 'utf8')), false, `${relativePath} must remain generic.`);
}

console.log('wp-codebox preview materialization contract smoke passed');
