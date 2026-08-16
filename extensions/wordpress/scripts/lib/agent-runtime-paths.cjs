'use strict';

/**
 * Resolve files inside a shared agent-runtime package from a WordPress
 * extension script.
 *
 * `agent-runtimes` is declared in `homeboy-extension-root.json` as a shared
 * asset, and Homeboy installs shared runtimes BESIDE the extensions directory
 * rather than inside it:
 *
 *   monorepo checkout   <repo>/wordpress                 -> <repo>/agent-runtimes
 *   installed layout    <homeboy>/extensions/wordpress   -> <homeboy>/agent-runtimes
 *
 * The two layouts differ by exactly one directory level, so a relative require
 * that is correct in a checkout resolves to a nonexistent
 * `<homeboy>/extensions/agent-runtimes` once the extension is installed. A
 * linked (symlinked) dev install hides this, because Node resolves a symlinked
 * module's `__dirname` back to the source checkout; only a copied install — a
 * fresh CI runner — takes the broken branch. That is what produced four
 * identical `MODULE_NOT_FOUND` shard bootstrap failures in #12585.
 *
 * Probing both layouts is what lets one script load the same runtime module
 * from either. When neither resolves, the caller gets the probed paths and a
 * remediation instead of a bare `MODULE_NOT_FOUND` stack.
 */

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const EXTENSION_ROOT = path.resolve(__dirname, '..', '..');

/**
 * Candidate `agent-runtimes` roots, most-specific first.
 *
 * @param {{ env?: NodeJS.ProcessEnv, extensionRoot?: string }} [options]
 * @return {string[]} Absolute `agent-runtimes` roots to probe, in order.
 */
function agentRuntimeRoots(options = {}) {
	const env = options.env || process.env;
	const extensionRoots = [];

	if (options.extensionRoot) {
		extensionRoots.push(path.resolve(options.extensionRoot));
	} else {
		if (env.HOMEBOY_EXTENSION_PATH) {
			extensionRoots.push(path.resolve(env.HOMEBOY_EXTENSION_PATH));
		}
		extensionRoots.push(EXTENSION_ROOT);
	}

	const roots = [];
	for (const extensionRoot of extensionRoots) {
		// Installed: <homeboy>/extensions/<extension> -> <homeboy>/agent-runtimes
		roots.push(path.resolve(extensionRoot, '..', '..', 'agent-runtimes'));
		// Checkout: <repo>/<extension> -> <repo>/agent-runtimes
		roots.push(path.resolve(extensionRoot, '..', 'agent-runtimes'));
	}

	return roots.filter((root, index) => roots.indexOf(root) === index);
}

/**
 * Absolute path to a file inside a shared agent runtime.
 *
 * @param {string}                                              relativePath Path relative to the `agent-runtimes` root,
 *                                                                           e.g. `wp-codebox/lib/wp-codebox-runtime-selection.js`.
 * @param {{ env?: NodeJS.ProcessEnv, extensionRoot?: string }} [options]
 * @return {string} Absolute path to the resolved runtime file.
 * @throws {Error} When the file is absent from every probed layout.
 */
function resolveAgentRuntimeFile(relativePath, options = {}) {
	const probed = agentRuntimeRoots(options).map((root) => path.join(root, relativePath));
	const resolved = probed.find((candidate) => fs.existsSync(candidate));
	if (resolved) {
		return resolved;
	}

	const error = new Error(
		[
			`Homeboy WordPress extension could not resolve shared agent runtime file '${relativePath}'.`,
			'Probed:',
			...probed.map((candidate) => `  - ${candidate}`),
			"Shared agent runtimes are declared in homeboy-extension-root.json and install beside the extensions directory (<homeboy>/agent-runtimes), not inside it. The installed extension payload is incomplete.",
			'Remediation: reinstall the extension so its shared runtime assets are materialized (`homeboy extension install wordpress`).',
		].join('\n')
	);
	error.code = 'HOMEBOY_AGENT_RUNTIME_FILE_MISSING';
	error.probed = probed;
	throw error;
}

/**
 * Require a module from a shared agent runtime.
 *
 * @param {string}                                              relativePath Path relative to the `agent-runtimes` root.
 * @param {{ env?: NodeJS.ProcessEnv, extensionRoot?: string }} [options]
 * @return {unknown} The runtime module exports.
 */
function requireAgentRuntimeModule(relativePath, options = {}) {
	return require(resolveAgentRuntimeFile(relativePath, options));
}

module.exports = { agentRuntimeRoots, resolveAgentRuntimeFile, requireAgentRuntimeModule };

// CLI mode: print the resolved path, or exit non-zero with the diagnostic.
// Setup uses this to fail an incomplete install before any shard is planned.
if (require.main === module) {
	const relativePath = process.argv[2];
	if (!relativePath) {
		process.stderr.write('Usage: agent-runtime-paths.cjs <path-relative-to-agent-runtimes>\n');
		process.exit(2);
	}
	try {
		process.stdout.write(`${resolveAgentRuntimeFile(relativePath)}\n`);
	} catch (error) {
		process.stderr.write(`${error.message}\n`);
		process.exit(1);
	}
}
