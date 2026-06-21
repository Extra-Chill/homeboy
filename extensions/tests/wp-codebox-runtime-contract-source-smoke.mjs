#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
	loadCanonicalRuntimeContractSource,
	loadRuntimeContractSource,
	providerRuntimeInvocationContract,
	runtimeContractManifest,
	runtimeContractSchemas,
	validateCanonicalRuntimeContractManifest,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-runtime-contract-source.js'));
const {
	WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA,
	WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
	WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS,
	WP_CODEBOX_RUNTIME_PROFILE_SCHEMA,
	wpCodeboxProviderRuntimeInvocationContract,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox'));

const fallbackManifest = runtimeContractManifest();
assert.equal(fallbackManifest.schema, 'wp-codebox/runtime-contract-manifest/v1');
assert.equal(fallbackManifest.schemas.runtimeBoundary.profile, 'wp-codebox/runtime-profile/v1');
assert.equal(fallbackManifest.schemas.runtimeBoundary.browserSessionProductDto, 'wp-codebox/browser-session-product-dto/v1');
assert.equal(fallbackManifest.schemas.artifact.resultEnvelope, 'wp-codebox/artifact-result-envelope/v1');
assert.deepEqual(fallbackManifest.providerRuntime, providerRuntimeInvocationContract());

const schemas = runtimeContractSchemas();
assert.equal(WP_CODEBOX_RUNTIME_PROFILE_SCHEMA, schemas.runtimeBoundary.profile);
assert.equal(WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA, schemas.artifact.resultEnvelope);
assert.equal(WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA, schemas.agentTask.runResult);
assert.equal(WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS.workspace_capture, schemas.runnerWorkspace.captureResult);
assert.deepEqual(wpCodeboxProviderRuntimeInvocationContract(), {
	...fallbackManifest.providerRuntime,
	result_schemas: {
		...fallbackManifest.providerRuntime.result_schemas,
		artifact_result_envelope: schemas.artifact.resultEnvelope,
	},
});

validateCanonicalRuntimeContractManifest(fallbackManifest);

assert.throws(
	() => validateCanonicalRuntimeContractManifest({
		...fallbackManifest,
		schemas: {
			...fallbackManifest.schemas,
			runtimeBoundary: {
				...fallbackManifest.schemas.runtimeBoundary,
				profile: 'wp-codebox/runtime-profile/v2',
			},
		},
	}),
	/mismatch at schemas\.runtimeBoundary\.profile/
);

const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'hbe-codebox-runtime-contract-'));
const canonicalModule = path.join(tempRoot, 'canonical-runtime-core.mjs');
fs.writeFileSync(canonicalModule, `
export function runtimeContractManifest() {
  return ${JSON.stringify(fallbackManifest, null, 2)};
}
export const RUNTIME_CONTRACT_NORMALIZERS = {
  runtimeProfile(input) { return { ...input, schema: 'wp-codebox/runtime-profile/v1' }; },
};
`);

const canonical = await loadCanonicalRuntimeContractSource({ wpCodeboxCoreModule: canonicalModule, required: true });
assert.equal(canonical.canonical, true);
assert.equal(canonical.source, pathToFileURL(canonicalModule).href);
assert.deepEqual(canonical.manifest, fallbackManifest);
assert.equal(typeof canonical.normalizers.runtimeProfile, 'function');

const loaded = await loadRuntimeContractSource({ wpCodeboxCoreModule: canonicalModule });
assert.equal(loaded.canonical, true);

const mismatchedModule = path.join(tempRoot, 'mismatched-runtime-core.mjs');
fs.writeFileSync(mismatchedModule, `
export function runtimeContractManifest() {
  const manifest = ${JSON.stringify(fallbackManifest, null, 2)};
  manifest.providerRuntime.tasks.workspaceCommand = 'wp-codebox.runner-workspace.command-v2';
  return manifest;
}
`);

await assert.rejects(
	() => loadCanonicalRuntimeContractSource({ wpCodeboxCoreModule: mismatchedModule, required: true }),
	(error) => {
		assert.match(error.message, /canonical runtime contract manifest is unavailable/);
		assert.match(error.wpCodeboxRuntimeContractErrors[0].message, /providerRuntime\.tasks\.workspaceCommand/);
		return true;
	}
);

console.log('wp-codebox runtime contract source smoke passed');
