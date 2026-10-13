'use strict';

/**
 * OpenCode account discovery for the shared capacity lookup
 * (`../../lib/provider-capacity`).
 *
 * OpenCode multi-account plugins keep a pool of OAuth accounts per provider
 * in `<provider>-oauth-accounts.json` beside the auth store and rotate between
 * them, so the route's capacity is the pool's capacity. Without a pool, the
 * single `auth.json` entry is the account.
 */
const fs = require('node:fs');
const path = require('node:path');
const { authStorePath, openCodeStoreSecretEnv } = require('./opencode-auth-plan');
const {
	CAPACITY_TIMEOUT_MS,
	oauthAccount,
	providerCapacity,
	readJson,
	storedAccount,
} = require('../../lib/provider-capacity');

const OPENCODE_CAPACITY_TIMEOUT_MS = CAPACITY_TIMEOUT_MS;

function openCodeProviderCapacity(provider, options = {}) {
	const env = options.env || process.env;
	const fileSystem = options.fs || fs;
	return providerCapacity(provider, {
		...options,
		scopePrefix: 'opencode',
		accounts: (name) => openCodeAccounts(name, env, fileSystem),
	});
}

function openCodeAccounts(provider, env, fileSystem) {
	const storePath = authStorePath(env);
	const pool = readJson(path.join(path.dirname(storePath), `${provider}-oauth-accounts.json`), fileSystem);
	const pooled = (Array.isArray(pool?.accounts) ? pool.accounts : [])
		.map((entry, index) => oauthAccount(entry, entry?.email || `${provider}#${index}`, Boolean(entry?.email)))
		.filter(Boolean);
	if (pooled.length) return pooled;

	const stored = storedAccount(readJson(storePath, fileSystem), provider);
	if (stored) return [stored];
	// Lab runners materialize OAuth store secrets as AI_PROVIDER_OPENCODE_<PROVIDER>_ACCESS.
	const accessEnv = openCodeStoreSecretEnv(provider).find((name) => name.endsWith('_ACCESS'));
	return accessEnv && typeof env[accessEnv] === 'string' && env[accessEnv]
		? [{ label: provider, credential: env[accessEnv] }]
		: [];
}

module.exports = {
	OPENCODE_CAPACITY_TIMEOUT_MS,
	openCodeProviderCapacity,
};
