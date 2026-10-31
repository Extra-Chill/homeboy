'use strict';

const assert = require('node:assert/strict');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= require('node:path').join(
	__dirname,
	'..',
	'..',
	'tests',
	'fixtures',
	'wp-codebox-core-runtime-contract.cjs'
);

const { codeboxRuntimeProfilePayload } = require('../lib/wp-codebox-runtime-profile');

const payload = codeboxRuntimeProfilePayload({
	runtimeRequirements: {
		component_contracts: [{
			slug: 'monorepo-component',
			path: '/workspace/repo/plugins/monorepo-component',
			pluginFile: 'monorepo-component/monorepo-component.php',
			loadAs: 'plugin',
		}],
		extra_plugins: [{
			slug: 'monorepo-component',
			source: '/workspace/repo/plugins/monorepo-component',
			path: '/workspace/repo/plugins/monorepo-component',
			sourceRoot: '/workspace/repo',
			sourceSubpath: 'plugins/monorepo-component',
			pluginFile: 'monorepo-component/monorepo-component.php',
			loadAs: 'plugin',
			activate: true,
		}],
	},
});

assert.equal(payload.component_contracts.length, 1);
assert.equal(payload.component_contracts[0].sourceRoot, undefined);
assert.equal(payload.component_contracts[0].sourceSubpath, undefined);
assert.equal(payload.component_contracts[0].activate, undefined);
assert.equal(payload.extra_plugins.length, 1);
assert.equal(payload.extra_plugins[0].sourceRoot, '/workspace/repo');
assert.equal(payload.extra_plugins[0].sourceSubpath, 'plugins/monorepo-component');
assert.equal(payload.extra_plugins[0].activate, true);

console.log('codebox-runtime-profile-smoke: ok');
