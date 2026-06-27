// Shared env gating for the WP Codebox runner smokes.
//
// Every wp-codebox runner smoke depends on the WP Codebox canonical runtime
// contract module. That module is resolved from HOMEBOY_WP_CODEBOX_CORE_MODULE
// (or an installed @automattic/wp-codebox-core package); when neither is present
// the in-repo fixture at tests/fixtures/wp-codebox-core-runtime-contract.cjs is
// used, matching wp-codebox-task-runner-smoke.js and
// wp-codebox-provider-plugin-env-override-smoke.mjs.
//
// When the resolved module cannot actually load the canonical contract (for
// example a stripped checkout or a bad explicit HOMEBOY_WP_CODEBOX_CORE_MODULE),
// the smoke skips cleanly instead of hard-crashing, so the local suite stays
// honest where the dependency is genuinely absent.

import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const fixtureCoreModule = path.join(repoRoot, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

// Resolve the WP Codebox canonical runtime contract module, defaulting to the
// in-repo fixture when HOMEBOY_WP_CODEBOX_CORE_MODULE is not already set. Returns
// the path that the smoke (and any subprocess it spawns) will use.
export function ensureWpCodeboxCoreModule() {
	process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= fixtureCoreModule;
	return process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE;
}

// Skip the smoke (clean exit 0) when the canonical runtime contract cannot be
// loaded from the resolved core module. Returns the resolved core module path so
// the caller can pass it on to subprocesses. `label` names the smoke in the skip
// message.
export function skipUnlessWpCodeboxCanonicalContract(label) {
	const coreModule = ensureWpCodeboxCoreModule();
	const { loadCanonicalRuntimeContractSourceSync } = require(
		path.join(repoRoot, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-runtime-contract-source.js')
	);
	const resolved = loadCanonicalRuntimeContractSourceSync({ required: false, wpCodeboxCoreModule: coreModule });
	if (!resolved) {
		console.log(
			`skipped: ${label} (WP Codebox canonical runtime contract unavailable at ${coreModule}; set HOMEBOY_WP_CODEBOX_CORE_MODULE)`
		);
		process.exit(0);
	}
	return coreModule;
}
