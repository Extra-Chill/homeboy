'use strict';

const fs = require('node:fs');
const path = require('node:path');

const OPENCODE_AUTH_STORE_PATH = '~/.local/share/opencode/auth.json';
const OPENCODE_STORE_SECRET_ENV_PREFIX = 'AI_PROVIDER_OPENCODE_';
const OPENCODE_STORE_SECRET_ENV_FIELDS = [
	['ACCESS', 'access'],
	['REFRESH', 'refresh'],
	['EXPIRES', 'expires'],
];
const OPENCODE_STORE_SECRET_ENV_NAME_PATTERN = /^AI_PROVIDER_OPENCODE_[A-Z0-9]+_(?:ACCESS|REFRESH|EXPIRES)$/;
// Explicit legacy file-handoff selector. Native provider names do not select an
// auth kind: the runtime resolves that from the selected account and environment.
const OPENAI_OAUTH_ACCOUNT = 'openai-oauth';
// Controller-refreshed Anthropic OAuth credential (homeboy#15783/#15874).
// The account selector is internal; Anthropic routes keep their native provider.
const ANTHROPIC_OAUTH_ACCOUNT = 'anthropic-oauth';
// Credential-selector accounts and the OpenCode provider each one routes to.
const STORE_OAUTH_ACCOUNT_PROVIDERS = {
	[OPENAI_OAUTH_ACCOUNT]: 'openai',
	[ANTHROPIC_OAUTH_ACCOUNT]: 'anthropic',
};
// Providers whose OAuth refresh chain the controller owns. Their store handoff
// declares access + expiry only, so a Lab runner never receives the single-use
// refresh token: two hosts holding one invalidate each other (homeboy#15783).
const CONTROLLER_REFRESHED_STORE_PROVIDERS = new Set(['anthropic']);

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
		: explicitKind && explicitKind !== metadata.auth_kind
			? nativeSource('selected authentication is not present in the native store')
		: metadata.source;
	const storeHandoff = authKind === 'oauth' && route.provider !== 'codex' && source.kind === 'opencode_auth_store';
	const secretEnv = route.provider === 'codex' && authKind === 'oauth'
		? [...CODEX_SECRET_ENV]
		: authKind === 'api_key' && route.provider === 'openai' && source.kind !== 'opencode_auth_store'
			? ['OPENAI_API_KEY']
		: storeHandoff ? openCodeStoreSecretEnv(route.provider) : [];
	const incompatibleSelection = route.provider === 'openai' && explicitKind && (
		metadata.auth_kind === 'unknown' ? explicitKind !== 'api_key' : explicitKind !== metadata.auth_kind
	);
	return {
		supported: !incompatibleSelection,
		...(incompatibleSelection ? { reason: 'The selected OpenAI authentication does not match the native account. Select a matching native account before running OpenCode.' } : {}),
		provider: route.provider,
		model: route.model,
		account_kind: route.provider === 'anthropic' && authKind === 'oauth'
			? ANTHROPIC_OAUTH_ACCOUNT
			: accountKind(route.provider, authKind),
		auth_kind: authKind,
		...(route.provider === 'anthropic' && authKind === 'oauth' && metadata.credential_expired ? { credential_expired: true } : {}),
		secret_env: secretEnv,
		secret_env_sources: storeHandoff ? openCodeStoreSecretEnvSources(route.provider)
			: source.kind === 'opencode_auth_store' && authKind === 'api_key' ? {} : secretSources(route.provider, authKind),
		source,
		metadata_only: true,
		...(source.handoff_supported === false ? { handoff_blocker: source.reason } : {}),
	};
}

// Only a selected scoped environment source needs environment credentials.
// Native accounts are authenticated by OpenCode's bounded readiness request and
// native request path; OAuth file-handoff env names are not API-key requirements.
function openCodeAuthPlanError(plan, env) {
	if (!plan.supported) return plan.reason;
	if (plan.provider === 'anthropic' && plan.auth_kind === 'oauth' && plan.credential_expired) {
		return 'The handed-off Anthropic OAuth credential has expired; re-login Anthropic on the controller and retry.';
	}
	if (plan.auth_kind === 'api_key' && plan.secret_env.includes('OPENAI_API_KEY') && !hasValue(env.OPENAI_API_KEY)) {
		return 'Selected OpenAI API-key authentication requires OPENAI_API_KEY.';
	}
	return null;
}

function selectedOpenCodeRoute(config = {}) {
	const configuredModel = stringValue(config.model);
	const separator = configuredModel.indexOf('/');
	const modelProvider = separator > 0 ? configuredModel.slice(0, separator) : '';
	const configuredProvider = stringValue(config.provider);
	// A store account is a credential selector for its provider's route, not a
	// separate OpenCode provider.
	const normalizedProvider = STORE_OAUTH_ACCOUNT_PROVIDERS[configuredProvider] || configuredProvider;
	return {
		provider: normalizedProvider || modelProvider,
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
			return {
				auth_kind: authKind,
				credential_expired: typeof entry.expires === 'number'
					&& entry.expires <= Date.now()
					&& !(typeof entry.refresh === 'string' && entry.refresh !== ''),
				source: { kind: 'opencode_auth_store', location: OPENCODE_AUTH_STORE_PATH, handoff_supported: true },
			};
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

function openCodeStoreSecretEnvKey(provider) {
	return String(provider || '').toUpperCase().replace(/[^A-Z0-9]/g, '_');
}

function openCodeStoreSecretEnvFields(provider) {
	return CONTROLLER_REFRESHED_STORE_PROVIDERS.has(provider)
		? OPENCODE_STORE_SECRET_ENV_FIELDS.filter(([suffix]) => suffix !== 'REFRESH')
		: OPENCODE_STORE_SECRET_ENV_FIELDS;
}

function openCodeStoreSecretEnv(provider) {
	const key = openCodeStoreSecretEnvKey(provider);
	return openCodeStoreSecretEnvFields(provider).map(([suffix]) => `${OPENCODE_STORE_SECRET_ENV_PREFIX}${key}_${suffix}`);
}

function openCodeStoreSecretEnvSources(provider) {
	const key = openCodeStoreSecretEnvKey(provider);
	return Object.fromEntries(openCodeStoreSecretEnvFields(provider).map(([suffix, field]) => [
		`${OPENCODE_STORE_SECRET_ENV_PREFIX}${key}_${suffix}`,
		{ source: 'json-file', path: OPENCODE_AUTH_STORE_PATH, field: `${provider}.${field}` },
	]));
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
	ANTHROPIC_OAUTH_ACCOUNT,
	CONTROLLER_REFRESHED_STORE_PROVIDERS,
	CODEX_SECRET_ENV,
	CODEX_SECRET_ENV_SOURCES,
	OPENAI_OAUTH_ACCOUNT,
	OPENCODE_AUTH_STORE_PATH,
	OPENCODE_STORE_SECRET_ENV_NAME_PATTERN,
	authMetadata,
	authStorePath,
	openCodeStoreSecretEnv,
	openCodeStoreSecretEnvSources,
	resolveOpenCodeAuthPlan,
	openCodeAuthPlanError,
	selectedOpenCodeRoute,
	effectiveOpenCodeModel,
};
