'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
	READINESS_REQUEST_SCHEMA,
	READINESS_RESULT_SCHEMA,
	boundedTimeout,
	resolveExecutable,
} = require('../../lib/cli-runtime-readiness');
const { resolveOpenCodeAuthPlan, selectedOpenCodeRoute } = require('./opencode-auth-plan');
const { openCodeProviderCapacity } = require('./opencode-provider-capacity');

const OPENCODE_READINESS_TIMEOUT_MS = 15_000;
const OPENCODE_READINESS_MAX_TIMEOUT_MS = 30_000;
const OPENCODE_READINESS_MAX_OUTPUT_BYTES = 16 * 1024;
const OPENCODE_AUTH_FAILURE_PATTERN = /\b(?:auth(?:entication)?|credential|login|token|unauthori[sz]ed|forbidden|account)\b/i;
const OPENCODE_ACCOUNT_BLOCKED_PATTERN = /\b(?:personal-team-blocked|spending limit|run out of credits|need a \w+ subscription|billing|insufficient credits)\b/i;
const OPENCODE_QUOTA_PATTERN = /\b(?:quota|rate limit|usage limit|spending limit|limit exhausted|too many requests|\b429\b)\b/i;
const OPENCODE_TRANSIENT_PATTERN = /\b(?:timeout|timed out|temporar(?:y|ily)|network|connect(?:ion)?|unavailable|service unavailable|\b5\d\d\b)\b/i;
const OPENCODE_UNSUPPORTED_MODEL_PATTERN = /\b(?:model|deployment|engine)\b[\s\S]{0,100}\b(?:unsupported|unavailable|not found|does not exist|unknown)\b/i;
const OPENCODE_UNSUPPORTED_PROBE_PATTERN = /\b(?:unknown|unrecognized|invalid) (?:argument|option|command)|\bnot supported\b/i;
const OPENCODE_READINESS_AGENT = 'homeboy-readiness';
const OPENCODE_READINESS_PROMPT = 'Reply with exactly READY. Do not access files, run commands, or make changes.';
const OPENCODE_SECRET_ENV_NAME_PATTERN = /(?:token|secret|api[_-]?key|credential|auth|access|refresh|expires)/i;

function openCodeRuntimeReadiness(request = {}, options = {}) {
	const config = objectValue(request.effective_config);
	const env = objectValue(options.env || process.env);
	const selected = selectedProviderModel(config || {});
	const authPlan = resolveOpenCodeAuthPlan(config || {}, { env });
	const identity = {
		runtime_id: 'opencode',
		provider_id: 'opencode.agent-task-executor',
		executable: '',
		version: '',
		provider: selected.provider,
		model: selected.model,
		credential_identity: credentialIdentity(env, authPlan),
	};
	if (request.schema !== READINESS_REQUEST_SCHEMA || !config) {
		return verdict('configuration_failure', identity, 'Provide a resolved effective_config using the provider readiness request contract.', false, 'invalid_readiness_request');
	}
	if (selected.error) {
		return verdict('configuration_failure', identity, selected.error, false, 'invalid_provider_model');
	}
	if (!authPlan.supported) {
		return verdict('configuration_failure', identity, authPlan.reason, false, 'unsupported_provider_route');
	}

	const command = config.runtime_bin || config.runtimeBin || config.command || 'opencode';
	if (typeof command !== 'string' || command.trim() === '') {
		return verdict('configuration_failure', identity, 'Configure executor.config.runtime_bin as a non-empty OpenCode executable.', false, 'invalid_command');
	}
	const executable = resolveExecutable(command.trim(), env, { fs: options.fs || fs });
	const executableIdentity = { ...identity, executable };
	if (!executable) {
		return verdict('configuration_failure', executableIdentity, 'Install OpenCode or configure executor.config.runtime_bin to an executable path.', false, 'executable_not_found');
	}

	const probe = options.spawnSync || spawnSync;
	const versionResult = runProbe(probe, executable, commandArgs(config, env, options), ['--version'], env, config);
	const versionFailure = probeFailure(versionResult, executableIdentity, 'version');
	if (versionFailure) return versionFailure;
	const version = firstLine(versionResult.stdout);
	if (!version) {
		return verdict('configuration_failure', executableIdentity, 'Use an OpenCode executable that reports a version through --version.', false, 'version_metadata_unavailable');
	}
	const versionIdentity = { ...executableIdentity, version };

	const authResult = runProbe(probe, executable, commandArgs(config, env, options), ['auth', 'list'], env, config);
	const authFailure = probeFailure(authResult, versionIdentity, 'auth_list');
	if (authFailure) return authFailure;
	if (!authenticatedProvider(authResult.stdout, selected.provider)) {
		return verdict('auth_failure', versionIdentity, `Authenticate the ${selected.provider} provider through \`opencode auth login\`, then retry.`, false, 'provider_credentials_missing');
	}

	const modelsResult = runProbe(probe, executable, commandArgs(config, env, options), ['models', selected.provider], env, config);
	const modelsFailure = probeFailure(modelsResult, versionIdentity, 'model_probe');
	if (modelsFailure) return modelsFailure;
	if (!listedModel(modelsResult.stdout, selected.provider, selected.model)) {
		return verdict('configuration_failure', versionIdentity, `Configure a model that OpenCode exposes for provider ${selected.provider}, then retry.`, false, 'model_not_available');
	}
	if (options.capacityExhausted) {
		// The plan usage endpoint already proved the account is out of capacity;
		// skip the inference probe instead of spending a request to rediscover it.
		return verdict('capacity', versionIdentity, 'Wait for the provider plan usage window to reset, then retry.', true, 'provider_capacity_exhausted');
	}
	const providerResult = runModelProbe(probe, executable, commandArgs(config, env, options), selected, env, config, options);
	if (providerResult.isolationError) {
		return verdict('indeterminate', versionIdentity, 'OpenCode could not create an isolated workspace for the provider readiness probe.', true, 'model_probe_isolation_unavailable');
	}
	const providerFailure = probeFailure(providerResult, versionIdentity, 'model_execution');
	if (providerFailure) return providerFailure;
	return verdict('ready', versionIdentity, `OpenCode provider ${selected.provider}/${selected.model} accepted the bounded readiness request.`, false, 'model_execution_ready');
}

/**
 * Readiness plus plan capacity: looks up the selected provider's usage
 * windows first so an exhausted plan is reported as `capacity` with its reset
 * time, and a healthy one carries its remaining capacity. Capacity lookups are
 * best-effort; a failed lookup never changes the readiness verdict.
 */
async function openCodeProviderReadiness(request = {}, options = {}) {
	const selected = selectedProviderModel(objectValue(request.effective_config) || {});
	const lookup = selected.error || !selected.provider
		? null
		: await openCodeProviderCapacity(selected.provider, {
			env: objectValue(options.env || process.env),
			fetch: options.fetch,
			fs: options.capacityFs,
			now: options.now,
		});
	const result = openCodeRuntimeReadiness(request, { ...options, capacityExhausted: Boolean(lookup?.exhausted) });
	if (lookup?.capacity) result.capacity = lookup.capacity;
	if (lookup?.accounts) result.capacity_accounts = lookup.accounts;
	if (lookup?.diagnostic) result.capacity_diagnostic = lookup.diagnostic;
	return result;
}

function selectedProviderModel(config = {}) {
	const route = selectedOpenCodeRoute(config);
	if (!route.provider || !route.model || !routeSegment(route.provider) || !routeSegment(route.model)) {
		return { provider: route.provider, model: route.model, error: 'Configure effective_config.model as provider/model so readiness can validate the selected route.' };
	}
	return { provider: route.provider, model: route.model, error: '' };
}

function commandArgs(config = {}, env = {}, options = {}) {
	const value = options.commandArgs || config.command_args || config.commandArgs || parseArgs(env.HOMEBOY_OPENCODE_COMMAND_ARGS);
	return Array.isArray(value) && value.every((item) => typeof item === 'string') ? value : [];
}

function parseArgs(value) {
	try {
		const parsed = JSON.parse(value || 'null');
		return Array.isArray(parsed) ? parsed : null;
	} catch {
		return null;
	}
}

function runProbe(probe, executable, args, command, env, config, cwd) {
	const result = probe(executable, [...args, ...command], {
		encoding: 'utf8',
		timeout: boundedTimeout(
			config.readiness_timeout_ms || OPENCODE_READINESS_TIMEOUT_MS,
			OPENCODE_READINESS_MAX_TIMEOUT_MS,
		),
		maxBuffer: OPENCODE_READINESS_MAX_OUTPUT_BYTES,
		env,
		...(cwd ? { cwd } : {}),
	});
	return {
		...result,
		stdout: redact(String(result.stdout || ''), env),
		stderr: redact(String(result.stderr || ''), env),
	};
}

function runModelProbe(probe, executable, args, selected, env, config, options) {
	const fileSystem = options.fs || fs;
	let cwd;
	let result;
	let cleanupFailed = false;
	try {
		cwd = fileSystem.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-readiness-'));
	} catch {
		return { isolationError: true, stdout: '', stderr: '' };
	}
	try {
		result = runProbe(probe, executable, args, [
			// Older OpenCode CLIs only emit provider errors to stderr with
			// --print-logs; interrupted probes are classified from that output.
			'run', '--print-logs', '--log-level', 'ERROR',
			'--model', `${selected.provider}/${selected.model}`, '--format', 'json',
			'--agent', OPENCODE_READINESS_AGENT, '--title', OPENCODE_READINESS_AGENT,
			OPENCODE_READINESS_PROMPT,
		], readinessProbeEnvironment(env), config, cwd);
	} finally {
		try {
			fileSystem.rmSync(cwd, { recursive: true, force: true });
		} catch {
			cleanupFailed = true;
		}
	}
	return cleanupFailed ? { isolationError: true, stdout: '', stderr: '' } : result;
}

function probeFailure(result, identity, probeName) {
	const output = `${result.stdout}\n${result.stderr}`;
	if (result.error?.code === 'ENOBUFS') {
		return verdict('configuration_failure', identity, 'Use an OpenCode CLI that keeps readiness command output within the declared bound.', false, `${probeName}_output_limit`);
	}
	if (result.error?.code === 'ETIMEDOUT' || result.signal) {
		// OpenCode retries provider quota and billing rejections internally and
		// logs each attempt to stderr, so a probe can time out on a definitive
		// provider verdict. Classify from the output captured before the kill.
		const providerVerdict = interruptedProviderVerdict(output, identity, probeName);
		if (providerVerdict) return providerVerdict;
		return verdict('transient_failure', identity, 'Retry the bounded OpenCode readiness probe; the local process did not complete.', true, `${probeName}_interrupted`);
	}
	if (result.error) {
		return verdict('transient_failure', identity, 'Retry the bounded OpenCode readiness probe after the local process error is resolved.', true, `${probeName}_error`);
	}
	if (result.status !== 0) {
		if (probeName === 'model_execution' && OPENCODE_ACCOUNT_BLOCKED_PATTERN.test(output)) {
			return verdict('provider_account_blocked', identity, 'Restore provider account access or billing for the selected model, then retry.', false, 'provider_account_blocked');
		}
		if (OPENCODE_QUOTA_PATTERN.test(output)) {
			return verdict('capacity', identity, 'Wait for the provider quota or rate limit to reset, then retry.', true, 'provider_quota_or_rate_limit');
		}
		if (OPENCODE_AUTH_FAILURE_PATTERN.test(output)) {
			return verdict('auth_failure', identity, 'Refresh the provider-owned OpenCode credentials, then retry.', false, 'authentication_rejected');
		}
		if (OPENCODE_TRANSIENT_PATTERN.test(output)) {
			return verdict('transient_failure', identity, 'Retry the OpenCode provider readiness probe after the provider is reachable.', true, `${probeName}_transient_failure`);
		}
		if (probeName === 'model_execution' && OPENCODE_UNSUPPORTED_MODEL_PATTERN.test(output)) {
			return verdict('configuration_failure', identity, 'Select a provider model supported by the authenticated account, then retry.', false, 'model_unsupported');
		}
		if (probeName === 'model_execution' && OPENCODE_UNSUPPORTED_PROBE_PATTERN.test(output)) {
			return verdict('indeterminate', identity, 'This OpenCode CLI cannot run the isolated model readiness probe; do not treat catalog discovery as provider readiness.', false, 'model_probe_unsupported');
		}
		if (probeName === 'model_execution') {
			return verdict('indeterminate', identity, 'OpenCode could not prove that the selected provider model is usable; do not treat catalog discovery as provider readiness.', true, 'model_probe_unclassified');
		}
		return verdict('configuration_failure', identity, 'Configure a compatible OpenCode CLI and provider route, then retry.', false, `${probeName}_rejected`);
	}
	return null;
}

function interruptedProviderVerdict(output, identity, probeName) {
	if (probeName !== 'model_execution') return null;
	if (OPENCODE_ACCOUNT_BLOCKED_PATTERN.test(output)) {
		return verdict('provider_account_blocked', identity, 'Restore provider account access or billing for the selected model, then retry.', false, 'provider_account_blocked');
	}
	if (OPENCODE_QUOTA_PATTERN.test(output)) {
		return verdict('capacity', identity, 'Wait for the provider quota or rate limit to reset, then retry.', true, 'provider_quota_or_rate_limit');
	}
	return null;
}

function readinessProbeEnvironment(env) {
	let config = {};
	try {
		config = objectValue(JSON.parse(env.OPENCODE_CONFIG_CONTENT || '{}')) || {};
	} catch {
		// Preserve malformed ambient config for OpenCode to report without replacing it.
		return env;
	}
	return {
		...env,
		OPENCODE_CONFIG_CONTENT: JSON.stringify({
			...config,
			agent: {
				...objectValue(config.agent),
				[OPENCODE_READINESS_AGENT]: {
					mode: 'primary',
					prompt: OPENCODE_READINESS_PROMPT,
					permission: { '*': 'deny' },
				},
			},
		}),
	};
}

function authenticatedProvider(output, provider) {
	const normalizedProvider = normalizeProvider(provider);
	return String(output).split(/\r?\n/).some((line) => normalizeProvider(line).includes(normalizedProvider));
}

function listedModel(output, provider, model) {
	return String(output).split(/\r?\n/).some((line) => line.trim() === `${provider}/${model}`);
}

function verdict(classification, identity, remediation, retryable, reason) {
	const cacheIdentity = {
		runtime_id: identity.runtime_id,
		provider_id: identity.provider_id,
		executable: identity.executable || '',
		version: identity.version || '',
		provider: identity.provider || '',
		model: identity.model || '',
		credential_identity: identity.credential_identity || '',
	};
	const cacheKey = `runtime-readiness:${crypto.createHash('sha256').update(JSON.stringify(cacheIdentity)).digest('hex')}`;
	return {
		schema: READINESS_RESULT_SCHEMA,
		ready: classification === 'ready',
		classification,
		retryable,
		message: remediation,
		remediation,
		reason,
		identity: { ...cacheIdentity, cache_key: cacheKey },
		cache_key: cacheKey,
	};
}

function credentialIdentity(env, authPlan = {}) {
	const values = Object.entries(env)
		.filter(([name]) => OPENCODE_SECRET_ENV_NAME_PATTERN.test(name))
		.sort(([left], [right]) => left.localeCompare(right));
	return crypto.createHash('sha256').update(JSON.stringify({
		environment: values,
		auth_plan: {
			provider: authPlan.provider || '',
			account_kind: authPlan.account_kind || '',
			auth_kind: authPlan.auth_kind || '',
			secret_env: authPlan.secret_env || [],
		},
		auth_store: authStoreIdentity(env),
		// Custom provider credentials and account routing can be configured here.
		opencode_config: configContentIdentity(env),
	})).digest('hex');
}

function configContentIdentity(env) {
	const content = env.OPENCODE_CONFIG_CONTENT;
	return typeof content === 'string'
		? crypto.createHash('sha256').update(content).digest('hex')
		: '';
}

function authStoreIdentity(env) {
	const dataHome = env.XDG_DATA_HOME || path.join(env.HOME || '', '.local', 'share');
	const authPath = path.join(dataHome, 'opencode', 'auth.json');
	try {
		const stat = fs.statSync(authPath);
		if (!stat.isFile() || stat.size > OPENCODE_READINESS_MAX_OUTPUT_BYTES) return '';
		return crypto.createHash('sha256').update(fs.readFileSync(authPath)).digest('hex');
	} catch {
		return '';
	}
}

function redact(value, env) {
	let redacted = value;
	for (const [name, secret] of Object.entries(env)) {
		if (secret && OPENCODE_SECRET_ENV_NAME_PATTERN.test(name)) redacted = redacted.split(String(secret)).join('[redacted]');
	}
	return redacted;
}

function firstLine(value) {
	return String(value || '').split(/\r?\n/).map((line) => line.trim()).find(Boolean) || '';
}

function routeSegment(value) {
	return /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(value);
}

function normalizeProvider(value) {
	return String(value).replace(/\x1B\[[0-?]*[ -/]*[@-~]/g, '').toLowerCase().replace(/[^a-z0-9]/g, '');
}

function objectValue(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : null;
}

module.exports = {
	OPENCODE_READINESS_MAX_OUTPUT_BYTES,
	OPENCODE_READINESS_TIMEOUT_MS,
	OPENCODE_READINESS_MAX_TIMEOUT_MS,
	openCodeProviderReadiness,
	openCodeRuntimeReadiness,
};
