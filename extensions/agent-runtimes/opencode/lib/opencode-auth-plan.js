'use strict';

const fs = require('node:fs');
const path = require('node:path');

const CODEX_SECRET_ENV = [
	'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
	'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
	'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
	'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
	'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];

const CODEX_SECRET_ENV_SOURCES = {
	AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: { source: 'json-file', path: '~/.codex/auth.json', field: 'tokens.access_token' },
	AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: { source: 'json-file', path: '~/.codex/auth.json', field: 'tokens.refresh_token' },
	AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: { source: 'json-file-jwt-expiration', path: '~/.codex/auth.json', field: 'tokens.access_token', fallback_fields: ['tokens.expires_at', 'tokens.expiresAt'] },
	AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: { source: 'json-file', path: '~/.codex/auth.json', field: 'tokens.account_id' },
	AI_PROVIDER_OPENAI_CODEX_FEDRAMP: { source: 'json-file', path: '~/.codex/auth.json', field: 'tokens.fedramp', value: 'false' },
};

function resolveOpenCodeAuthPlan(config = {}, options = {}) {
	const route = selectedOpenCodeRoute(config);
	const metadata = authMetadata(route.provider, options.env || process.env, options);
	const explicitKind = explicitAuthKind(config);
	const env = options.env || process.env;
	const codexEnvPresent = route.provider === 'codex' && hasValue(env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN);
	const inferredKind = metadata.auth_kind !== 'unknown' ? metadata.auth_kind : '';
	const authKind = explicitKind || inferredKind || (codexEnvPresent ? 'oauth' : route.provider === 'codex' ? 'oauth' : route.provider === 'openai' && hasValue(env.OPENAI_API_KEY) ? 'api_key' : 'unknown');
	const source = authKind === 'api_key' && route.provider === 'openai' && hasValue(env.OPENAI_API_KEY)
		? { kind: 'scoped_secret_env', location: 'OPENAI_API_KEY', handoff_supported: true }
		: authKind === 'oauth' && route.provider === 'codex' && codexEnvPresent
			? { kind: 'scoped_secret_env', location: 'AI_PROVIDER_OPENAI_CODEX_*', handoff_supported: true }
		: metadata.source;
	const secretEnv = route.provider === 'codex' && authKind === 'oauth'
		? [...CODEX_SECRET_ENV]
		: authKind === 'api_key' && route.provider === 'openai'
			? ['OPENAI_API_KEY']
			: [];
	return {
		supported: true,
		provider: route.provider,
		model: route.model,
		account_kind: accountKind(route.provider, authKind),
		auth_kind: authKind,
		secret_env: secretEnv,
		secret_env_sources: secretSources(route.provider, authKind),
		source,
		metadata_only: true,
		...(source.handoff_supported === false ? { handoff_blocker: source.reason } : {}),
	};
}

function selectedOpenCodeRoute(config = {}) {
	const configuredModel = stringValue(config.model);
	const separator = configuredModel.indexOf('/');
	const modelProvider = separator > 0 ? configuredModel.slice(0, separator) : '';
	const configuredProvider = stringValue(config.provider);
	return {
		provider: configuredProvider || modelProvider,
		model: separator > 0 ? configuredModel.slice(separator + 1) : configuredModel,
	};
}

function effectiveOpenCodeModel(config = {}, fallbackModel = '') {
	const configuredModel = stringValue(config.model) || fallbackModel;
	const configuredProvider = stringValue(config.provider);
	const modelProvider = configuredModel.split('/')[0];
	// A provider override selects the credential account. Preserve custom native
	// model ids unless the request is the declared OpenAI route being overridden.
	if (configuredProvider && configuredModel.includes('/') && modelProvider !== configuredProvider && modelProvider !== 'openai') {
		return configuredModel;
	}
	const route = selectedOpenCodeRoute({ ...config, model: configuredModel });
	return route.provider && route.model ? `${route.provider}/${route.model}` : route.model;
}

function explicitAuthKind(config = {}) {
	const account = objectValue(config.account || config.account_metadata || config.accountMetadata);
	return normalizeAuthKind(
		config.auth_kind,
		config.authKind,
		config.credential_kind,
		config.credentialKind,
		account?.auth_kind,
		account?.authKind,
		account?.type,
	);
}

function authMetadata(provider, env, options = {}) {
	if (!provider) return { auth_kind: 'unknown', source: nativeSource('missing provider metadata') };
	const authPath = options.authPath || authStorePath(env);
	try {
		const stat = fs.statSync(authPath);
		if (!stat.isFile() || stat.size > 64 * 1024) return { auth_kind: 'unknown', source: nativeSource('auth store metadata unavailable') };
		const entry = objectValue(JSON.parse(fs.readFileSync(authPath, 'utf8'))[provider]);
		const authKind = normalizeAuthKind(entry?.type);
		if (authKind) {
			return { auth_kind: authKind, source: { kind: 'opencode_auth_store', location: '~/.local/share/opencode/auth.json', handoff_supported: false, reason: 'Provider-owned OpenCode auth metadata cannot be copied into a runner; authenticate the selected account on the runner.' } };
		}
	} catch {
		// A missing store is a truthful unknown account, not a reason to reject native providers.
	}
	return { auth_kind: 'unknown', source: nativeSource('provider auth metadata unavailable') };
}

function authStorePath(env) {
	return path.join(env.XDG_DATA_HOME || path.join(env.HOME || '', '.local', 'share'), 'opencode', 'auth.json');
}

function normalizeAuthKind(...values) {
	for (const value of values) {
		const normalized = stringValue(value).toLowerCase();
		if (normalized === 'api' || normalized === 'api_key' || normalized === 'apikey' || normalized === 'key') return 'api_key';
		if (normalized === 'oauth' || normalized === 'oauth2') return 'oauth';
		if (normalized === 'wellknown' || normalized === 'well_known') return 'well_known';
	}
	return '';
}

function secretSources(provider, authKind) {
	if (provider === 'openai' && authKind === 'api_key') return { OPENAI_API_KEY: { source: 'environment', env: 'OPENAI_API_KEY' } };
	if (provider === 'codex' && authKind === 'oauth') return clone(CODEX_SECRET_ENV_SOURCES);
	return {};
}

function nativeSource(reason) {
	return { kind: 'provider_owned', location: 'OpenCode provider auth', handoff_supported: false, reason };
}

function accountKind(provider, authKind) {
	return authKind === 'unknown' ? `${provider || 'unknown'}_unknown` : `${provider || 'unknown'}_${authKind}`;
}

function hasValue(value) {
	return typeof value === 'string' && value !== '';
}

function stringValue(value) {
	return typeof value === 'string' ? value.trim() : '';
}

function objectValue(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : null;
}

function clone(value) {
	return JSON.parse(JSON.stringify(value));
}

module.exports = {
	CODEX_SECRET_ENV,
	CODEX_SECRET_ENV_SOURCES,
	authMetadata,
	resolveOpenCodeAuthPlan,
	selectedOpenCodeRoute,
	effectiveOpenCodeModel,
};
