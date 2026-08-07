'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const READINESS_REQUEST_SCHEMA = 'homeboy/agent-task-provider-readiness-request/v1';
const READINESS_RESULT_SCHEMA = 'homeboy/agent-task-provider-readiness-result/v1';
const DEFAULT_TIMEOUT_MS = 3_000;
const MAX_TIMEOUT_MS = 5_000;
const AUTH_FAILURE_PATTERN = /\b(?:auth(?:entication)?|credential|login|token|unauthori[sz]ed|forbidden)\b/i;

function cliRuntimeReadiness(request = {}, spec = {}, options = {}) {
	const config = objectValue(request.effective_config);
	const env = objectValue(options.env || process.env);
	const command = config.command || env[spec.commandEnv] || spec.defaultCommand;
	const model = selectedModel(config);
	const base = {
		runtime_id: spec.runtimeId,
		provider_id: spec.providerId,
		executable: '',
		version: '',
		model,
		environment_identity: environmentIdentity(env, spec.identityEnv),
	};

	if (request.schema !== READINESS_REQUEST_SCHEMA || !request.effective_config || !objectValue(request.effective_config)) {
		return verdict('deterministic_incompatibility', base, 'Provide a resolved effective_config using the provider readiness request contract.', false, 'invalid_readiness_request');
	}
	if (typeof command !== 'string' || command.trim() === '') {
		return verdict('deterministic_incompatibility', base, `Configure a non-empty ${spec.commandConfigKey || 'command'} executable.`, false, 'invalid_command');
	}

	const executable = resolveExecutable(command.trim(), env, options);
	const identity = { ...base, executable };
	if (!executable) {
		return verdict('deterministic_incompatibility', identity, `Install ${spec.label} or configure ${spec.commandConfigKey || 'command'} to an executable path.`, false, 'executable_not_found');
	}

	const missingAuth = requiredEnv(spec.requiredSecretEnv).filter((name) => !env[name]);
	if (missingAuth.length > 0) {
		return verdict('auth_failure', identity, `Resolve the required ${spec.label} credentials through the declared secret environment plan, then retry.`, false, 'required_credentials_missing', { missing_secret_env: missingAuth });
	}

	const probe = (options.spawnSync || spawnSync)(executable, spec.versionArgs || ['--version'], {
		encoding: 'utf8',
		timeout: boundedTimeout(config.readiness_timeout_ms || spec.timeoutMs),
		maxBuffer: 16 * 1024,
		env,
	});
	const output = `${probe.stdout || ''}\n${probe.stderr || ''}`.trim();
	if (probe.error) {
		if (probe.error.code === 'ETIMEDOUT' || probe.error.code === 'EAGAIN' || probe.error.code === 'EMFILE') {
			return verdict('transient_failure', identity, `Retry the bounded ${spec.label} version probe; the local runtime process was temporarily unavailable.`, true, probe.error.code);
		}
		return verdict('transient_failure', identity, `Retry the bounded ${spec.label} version probe after the local process error is resolved.`, true, 'version_probe_error');
	}
	if (probe.signal) {
		return verdict('transient_failure', identity, `Retry the bounded ${spec.label} version probe; the local runtime process was interrupted.`, true, 'version_probe_interrupted');
	}
	if (AUTH_FAILURE_PATTERN.test(output)) {
		return verdict('auth_failure', identity, `Refresh ${spec.label} credentials through the provider-owned authentication flow, then retry.`, false, 'authentication_rejected');
	}
	if (probe.status !== 0) {
		return verdict('deterministic_incompatibility', identity, `Install a ${spec.label} CLI version that supports ${spec.versionArgs?.join(' ') || '--version'}, or configure a compatible executable.`, false, 'version_probe_rejected');
	}
	const version = firstLine(probe.stdout || probe.stderr);
	if (!version) {
		return verdict('unknown_metadata', identity, `Use a ${spec.label} executable that reports a version through ${spec.versionArgs?.join(' ') || '--version'} before fanout.`, false, 'version_metadata_unavailable');
	}
	return verdict('ready', { ...identity, version }, `${spec.label} runtime is ready.`, false, 'ready');
}

function verdict(classification, identity, remediation, retryable, reason, extra = {}) {
	const cacheIdentity = {
		runtime_id: identity.runtime_id,
		provider_id: identity.provider_id,
		executable: identity.executable || '',
		version: identity.version || '',
		model: identity.model || '',
		environment_identity: identity.environment_identity || '',
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
		identity: {
			runtime_id: cacheIdentity.runtime_id,
			provider_id: cacheIdentity.provider_id,
			executable: cacheIdentity.executable,
			version: cacheIdentity.version,
			model: cacheIdentity.model,
			cache_key: cacheKey,
		},
		cache_key: cacheKey,
		...extra,
	};
}

function resolveExecutable(command, env, options = {}) {
	if (isPathLike(command)) {
		const resolved = path.resolve(expandHome(command, env));
		return executableFile(resolved, options) ? resolved : '';
	}
	for (const directory of String(env.PATH || '').split(path.delimiter)) {
		const candidate = path.join(directory || '.', command);
		if (executableFile(candidate, options)) return candidate;
	}
	return '';
}

function executableFile(file, options) {
	try {
		const fileSystem = options.fs || fs;
		fileSystem.accessSync(file, fs.constants.X_OK);
		return fileSystem.statSync(file).isFile();
	} catch {
		return false;
	}
}

function selectedModel(config) {
	return typeof config.model === 'string' && config.model.trim() ? config.model.trim() : '';
}

function environmentIdentity(env, names) {
	const entries = [...new Set(Array.isArray(names) ? names : [])]
		.filter((name) => typeof name === 'string' && name)
		.sort()
		.map((name) => [name, env[name] === undefined ? null : String(env[name])]);
	return crypto.createHash('sha256').update(JSON.stringify(entries)).digest('hex');
}

function boundedTimeout(value) {
	const timeout = Number(value);
	return Number.isFinite(timeout) && timeout > 0
		? Math.min(Math.floor(timeout), MAX_TIMEOUT_MS)
		: DEFAULT_TIMEOUT_MS;
}

function requiredEnv(value) {
	return Array.isArray(value) ? value : [];
}

function objectValue(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : null;
}

function firstLine(value) {
	return String(value || '').split(/\r?\n/).map((line) => line.trim()).find(Boolean) || '';
}

function isPathLike(value) {
	return value.startsWith('.') || value.startsWith('/') || value.startsWith('~/');
}

function expandHome(value, env) {
	return value.startsWith('~/') ? path.join(env.HOME || '', value.slice(2)) : value;
}

module.exports = {
	DEFAULT_TIMEOUT_MS,
	MAX_TIMEOUT_MS,
	READINESS_REQUEST_SCHEMA,
	READINESS_RESULT_SCHEMA,
	cliRuntimeReadiness,
	boundedTimeout,
	environmentIdentity,
	resolveExecutable,
};
