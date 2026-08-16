'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	agentRuntimeRoots,
	resolveAgentRuntimeFile,
	requireAgentRuntimeModule,
} = require('../scripts/lib/agent-runtime-paths.cjs');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-agent-runtime-paths-'));

// Installed layout: <homeboy>/extensions/wordpress alongside <homeboy>/agent-runtimes.
const homeboyRoot = path.join(root, 'config', 'homeboy');
const installedExtension = path.join(homeboyRoot, 'extensions', 'wordpress');
const installedRuntimeFile = path.join(homeboyRoot, 'agent-runtimes', 'wp-codebox', 'lib', 'selection.js');
fs.mkdirSync(installedExtension, { recursive: true });
fs.mkdirSync(path.dirname(installedRuntimeFile), { recursive: true });
fs.writeFileSync(installedRuntimeFile, 'module.exports = { layout: "installed" };\n');

assert.equal(
	resolveAgentRuntimeFile('wp-codebox/lib/selection.js', { extensionRoot: installedExtension }),
	installedRuntimeFile
);
assert.equal(
	requireAgentRuntimeModule('wp-codebox/lib/selection.js', { extensionRoot: installedExtension }).layout,
	'installed'
);

// A monorepo checkout keeps the shared tree one level closer to the extension.
const checkoutRoot = path.join(root, 'homeboy-extensions');
const checkoutExtension = path.join(checkoutRoot, 'wordpress');
const checkoutRuntimeFile = path.join(checkoutRoot, 'agent-runtimes', 'wp-codebox', 'lib', 'selection.js');
fs.mkdirSync(checkoutExtension, { recursive: true });
fs.mkdirSync(path.dirname(checkoutRuntimeFile), { recursive: true });
fs.writeFileSync(checkoutRuntimeFile, 'module.exports = { layout: "checkout" };\n');

assert.equal(
	resolveAgentRuntimeFile('wp-codebox/lib/selection.js', { extensionRoot: checkoutExtension }),
	checkoutRuntimeFile
);

// HOMEBOY_EXTENSION_PATH is honored ahead of this script's own location.
assert.equal(
	resolveAgentRuntimeFile('wp-codebox/lib/selection.js', {
		env: { HOMEBOY_EXTENSION_PATH: installedExtension },
	}),
	installedRuntimeFile
);

// Both layouts are probed for every candidate extension root, installed first.
assert.deepEqual(agentRuntimeRoots({ extensionRoot: installedExtension }), [
	path.join(homeboyRoot, 'agent-runtimes'),
	path.join(homeboyRoot, 'extensions', 'agent-runtimes'),
]);

// The reported #12585 failure resolved to <homeboy>/extensions/agent-runtimes and
// produced a bare MODULE_NOT_FOUND. An absent runtime must instead name every
// probed path and how to repair the install.
const emptyExtension = path.join(root, 'empty', 'extensions', 'wordpress');
fs.mkdirSync(emptyExtension, { recursive: true });
assert.throws(
	() => resolveAgentRuntimeFile('wp-codebox/lib/wp-codebox-runtime-selection.js', { extensionRoot: emptyExtension }),
	(error) => {
		assert.equal(error.code, 'HOMEBOY_AGENT_RUNTIME_FILE_MISSING');
		assert.match(error.message, /wp-codebox\/lib\/wp-codebox-runtime-selection\.js/);
		assert.match(error.message, /homeboy extension install wordpress/);
		for (const probed of agentRuntimeRoots({ extensionRoot: emptyExtension })) {
			assert.ok(
				error.message.includes(probed),
				`expected diagnostic to name probed root ${probed}`
			);
		}
		return true;
	}
);

// The real checkout resolves the module the PHPUnit adapter needs.
const selection = requireAgentRuntimeModule('wp-codebox/lib/wp-codebox-runtime-selection.js');
assert.equal(typeof selection.preflightWpCodeboxCommand, 'function');

fs.rmSync(root, { recursive: true, force: true });

console.log('agent runtime paths smoke passed');
