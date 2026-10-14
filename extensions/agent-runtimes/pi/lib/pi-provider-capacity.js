'use strict';

/**
 * Pi account discovery for the shared capacity lookup
 * (`../../lib/provider-capacity`, homeboy-extensions#2940).
 *
 * Pi keeps its logins in `<agent dir>/auth.json` (`PI_CODING_AGENT_DIR`, default
 * `~/.pi/agent`), keyed by provider in the same `{ type, access, expires }`
 * shape the shared lookup reads. The account label comes from the provider's
 * profile endpoint, so operators see which login a runner is using.
 *
 * Pi is a third-party client of subscription OAuth: Anthropic bills its
 * requests against the account's extra usage, not the plan windows (an
 * account with plan headroom but extra usage disabled answers
 * "You're out of extra usage"). The lookup therefore gates Anthropic on
 * extra usage.
 */
const fs = require('node:fs');
const path = require('node:path');
const { providerCapacity, readJson, storedAccount } = require('../../lib/provider-capacity');
const { piAgentDir } = require('./pi-agent-dir');

function piAuthStorePath(env = process.env, config = {}) {
	return path.join(piAgentDir(env, config), 'auth.json');
}

/** Pi routes are `provider/model`; the provider names the auth store entry. */
function piRouteProvider(model = '') {
	const [provider, ...rest] = String(model).split('/');
	return provider && rest.length ? provider : '';
}

function piProviderCapacity(provider, options = {}) {
	const env = options.env || process.env;
	const config = options.config || {};
	const fileSystem = options.fs || fs;
	return providerCapacity(provider, {
		...options,
		scopePrefix: 'pi',
		billing: provider === 'anthropic' ? 'extra_usage' : undefined,
		accounts: (name) => {
			const account = storedAccount(readJson(piAuthStorePath(env, config), fileSystem), name);
			return account ? [account] : [];
		},
	});
}

module.exports = {
	piAgentDir,
	piAuthStorePath,
	piProviderCapacity,
	piRouteProvider,
};
