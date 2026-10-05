'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
	AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
	agentTaskProviderContractFields,
} = require('../../../agent-task-contracts');
const {
	artifactDirectory,
	cliAgentTaskSpawnEnv,
	createCliAgentTaskExecutor,
	safeFileSegment,
} = require('../../lib/cli-agent-task-executor');
const { cliRuntimeReadiness } = require('../../lib/cli-runtime-readiness');
const { requiredOutputInstructions, structuredOutputsFromTexts } = require('../../lib/declared-outputs');

const PI_PROVIDER_ID = 'pi.agent-task-executor';
const PI_PROVIDER_LABEL = 'Pi agent task executor';
const PI_BACKEND = 'pi';
const PI_DEFAULT_COMMAND = 'pi';
// JSON mode: one invocation, a JSONL event stream on stdout, exit when done.
const PI_DEFAULT_COMMAND_ARGS = ['--mode', 'json'];
const DEFAULT_TIMEOUT_SECONDS = 1200;
// Linux limits a single argv string to 128 KiB; longer instructions go through a file.
const MAX_INLINE_INSTRUCTIONS_BYTES = 100 * 1024;
// Pi owns its credentials (its auth file under the agent dir, or provider
// API key env). The agent dir location and any provider keys a host declares
// pass through; nothing is required by default.
const PI_ENV_ALLOWLIST = ['HOMEBOY_PI_COMMAND', 'HOMEBOY_PI_COMMAND_ARGS', 'PI_CODING_AGENT_DIR'];

const PI_READINESS_INVOCATION = {
	schema: 'homeboy/command-invocation/v1',
	argv: ['node', '{{runtime_path}}/scripts/agent/homeboy-pi-provider-readiness.cjs'],
	env_allowlist: [...PI_ENV_ALLOWLIST],
	display: 'node {{runtime_path}}/scripts/agent/homeboy-pi-provider-readiness.cjs',
};

const PI_CAPABILITIES = [
	'cli_runtime',
	'workspace_materialization',
	'structured_outcome',
	'provider_owned_auth',
	'provider_owned_session',
];

function providerContract(options = {}) {
	return {
		schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
		id: options.id || PI_PROVIDER_ID,
		label: options.label || PI_PROVIDER_LABEL,
		backend: PI_BACKEND,
		runtime: PI_BACKEND,
		invocation: options.invocation || {
			schema: 'homeboy/command-invocation/v1',
			argv: ['node', '{{runtime_path}}/scripts/agent/homeboy-pi-agent-task-executor.cjs'],
			display: 'node {{runtime_path}}/scripts/agent/homeboy-pi-agent-task-executor.cjs',
		},
		readiness_invocation: options.readinessInvocation || PI_READINESS_INVOCATION,
		...agentTaskProviderContractFields(),
		secret_env_requirements: [],
		capabilities: [...PI_CAPABILITIES],
		workspace_materialization: {
			cwd: 'request_workspace',
			requires_git: false,
			write_scope: 'workspace',
		},
		provider_defaults: {
			pi: {
				command: PI_DEFAULT_COMMAND,
				command_args: [...PI_DEFAULT_COMMAND_ARGS],
			},
		},
		lifecycle: {
			completion: 'synchronous_process',
			cancellation: 'process_signal',
		},
		status: 'experimental',
		integration_contract: 'homeboy-pi-agent-task/v1',
	};
}

function piRuntimeReadiness(request = {}, options = {}) {
	return cliRuntimeReadiness(request, {
		runtimeId: PI_BACKEND,
		providerId: PI_PROVIDER_ID,
		label: 'Pi',
		defaultCommand: PI_DEFAULT_COMMAND,
		commandEnv: 'HOMEBOY_PI_COMMAND',
		commandConfigKey: 'executor.config.command',
		requiredSecretEnv: [],
		identityEnv: PI_ENV_ALLOWLIST,
	}, options);
}

function resolveCommandSpec(config = {}, options = {}) {
	const configuredCommand = options.command || config.command || process.env.HOMEBOY_PI_COMMAND || PI_DEFAULT_COMMAND;
	const configuredArgs = options.commandArgs || config.command_args || parseEnvCommandArgs() || PI_DEFAULT_COMMAND_ARGS;
	if (typeof configuredCommand !== 'string' || configuredCommand.trim() === '') {
		return { error: 'executor.config.command must be a non-empty string when provided.' };
	}
	if (!Array.isArray(configuredArgs) || configuredArgs.some((arg) => typeof arg !== 'string')) {
		return { error: 'executor.config.command_args must be an array of strings when provided.' };
	}
	return { command: configuredCommand.trim(), args: configuredArgs };
}

function parseEnvCommandArgs() {
	if (!process.env.HOMEBOY_PI_COMMAND_ARGS) {
		return null;
	}
	try {
		const value = JSON.parse(process.env.HOMEBOY_PI_COMMAND_ARGS);
		return Array.isArray(value) ? value : null;
	} catch {
		return null;
	}
}

function requestedModel(request = {}, config = {}) {
	return config.model || request.executor?.model || request.model || '';
}

/**
 * The prompt argument. Instructions too long for one argv entry are written to
 * a file the agent is told to read; the file lives with the run's artifacts.
 */
function promptArgument(request = {}, config = {}) {
	// Declared outputs (e.g. review_form) are asked for in the final answer.
	const instructions = `${request.instructions || ''}${requiredOutputInstructions(request)}`;
	if (Buffer.byteLength(instructions) <= MAX_INLINE_INSTRUCTIONS_BYTES) {
		return instructions;
	}
	const dir = artifactDirectory(request, config) || fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-pi-'));
	const file = path.join(dir, `${safeFileSegment(request.task_id)}-pi-instructions.md`);
	fs.mkdirSync(path.dirname(file), { recursive: true });
	fs.writeFileSync(file, instructions);
	return `Your full task instructions are in ${file}. Read that file first, then carry out the instructions exactly.`;
}

/** Pi session files go next to the run's artifacts when there is a place for them. */
function sessionArgs(request = {}, config = {}) {
	const dir = artifactDirectory(request, config);
	return dir ? ['--session-dir', path.join(dir, 'pi-session')] : ['--no-session'];
}

/**
 * Read Pi's JSONL event stream (`--mode json`). Records are split on LF only;
 * unparseable lines are counted and skipped.
 */
function parsePiEventStream(stdout = '') {
	const summary = {
		events: 0,
		invalidLines: 0,
		turns: 0,
		toolCalls: 0,
		toolErrors: 0,
		finalAssistant: null,
		finalText: '',
		retryFailure: '',
		settled: false,
		usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: 0 },
	};
	for (const rawLine of String(stdout).split('\n')) {
		const line = rawLine.endsWith('\r') ? rawLine.slice(0, -1) : rawLine;
		if (!line.trim()) {
			continue;
		}
		let event;
		try {
			event = JSON.parse(line);
		} catch {
			summary.invalidLines += 1;
			continue;
		}
		if (!event || typeof event !== 'object') {
			continue;
		}
		summary.events += 1;
		switch (event.type) {
			case 'turn_end':
				summary.turns += 1;
				break;
			case 'tool_execution_end':
				summary.toolCalls += 1;
				if (event.isError) {
					summary.toolErrors += 1;
				}
				break;
			case 'auto_retry_end':
				if (event.success === false) {
					summary.retryFailure = String(event.finalError || 'Automatic retry failed.');
				}
				break;
			case 'agent_settled':
				summary.settled = true;
				break;
			case 'message_end': {
				const message = event.message;
				if (message?.role !== 'assistant') {
					break;
				}
				summary.finalAssistant = message;
				const usage = message.usage || {};
				summary.usage.input += Number(usage.input) || 0;
				summary.usage.output += Number(usage.output) || 0;
				summary.usage.cacheRead += Number(usage.cacheRead) || 0;
				summary.usage.cacheWrite += Number(usage.cacheWrite) || 0;
				summary.usage.totalTokens += Number(usage.totalTokens) || 0;
				summary.usage.cost += Number(usage.cost?.total) || 0;
				const text = Array.isArray(message.content)
					? message.content.filter((block) => block?.type === 'text').map((block) => block.text).join('\n').trim()
					: '';
				if (text) {
					summary.finalText = text;
				}
				break;
			}
			default:
				break;
		}
	}
	return summary;
}

function piMetadata(context, stream, extra = {}) {
	const final = stream.finalAssistant || {};
	return {
		command: context.commandSpec.command,
		cwd: context.cwd,
		...(final.provider ? { provider: final.provider } : {}),
		...(final.model ? { model: final.model } : {}),
		...(final.stopReason ? { stop_reason: final.stopReason } : {}),
		turns: stream.turns,
		tool_calls: stream.toolCalls,
		tool_errors: stream.toolErrors,
		usage: {
			input_tokens: stream.usage.input,
			output_tokens: stream.usage.output,
			cache_read_tokens: stream.usage.cacheRead,
			cache_write_tokens: stream.usage.cacheWrite,
			total_tokens: stream.usage.totalTokens,
			cost_usd: Number(stream.usage.cost.toFixed(6)),
		},
		...(stream.invalidLines ? { invalid_event_lines: stream.invalidLines } : {}),
		...extra,
	};
}

function finalTextSummary(text) {
	if (!text) {
		return '';
	}
	const firstParagraph = text.split(/\n\s*\n/)[0].trim();
	return firstParagraph.length > 400 ? `${firstParagraph.slice(0, 399)}…` : firstParagraph;
}

function stderrTail(spawnResult) {
	return String(spawnResult.stderr || '').trim().split('\n').slice(-5).join('\n');
}

/** Map a completed Pi run (exit 0) onto an outcome from its final assistant message. */
function successFromStream(context) {
	const stream = parsePiEventStream(context.spawnResult.stdout);
	const final = stream.finalAssistant;
	if (!final) {
		return {
			status: 'provider_error',
			failure_classification: 'provider',
			failure_code: 'agent_task.pi_no_result',
			summary: 'Pi exited without producing an assistant message.',
			diagnostics: [{
				classification: 'provider',
				message: stream.events === 0
					? 'Pi produced no JSON events; check that command_args include --mode json.'
					: 'Pi produced events but no completed assistant message.',
			}],
			metadata: piMetadata(context, stream, { exit_code: 0 }),
		};
	}
	if (final.stopReason === 'error' || final.stopReason === 'aborted' || stream.retryFailure) {
		const message = final.errorMessage || stream.retryFailure || `Pi stopped with reason ${final.stopReason}.`;
		return {
			status: 'provider_error',
			failure_classification: 'provider',
			failure_code: final.stopReason === 'aborted' ? 'agent_task.pi_aborted' : 'agent_task.pi_model_error',
			summary: `Pi model request failed: ${message}`.slice(0, 500),
			diagnostics: [{ classification: 'provider', message }],
			metadata: piMetadata(context, stream, { exit_code: 0 }),
		};
	}
	if (final.stopReason === 'length') {
		return {
			status: 'failed',
			failure_classification: 'execution_failed',
			failure_code: 'agent_task.pi_output_limit',
			summary: 'Pi stopped at the model output limit before finishing.',
			diagnostics: [{ classification: 'execution_failed', message: 'The final assistant message ended with stopReason "length".' }],
			metadata: piMetadata(context, stream, { exit_code: 0 }),
		};
	}
	const structured = structuredOutputsFromTexts([stream.finalText], context.request, { runtime: 'pi', label: 'Pi' });
	const missing = structured.missingRequiredOutputs || [];
	return {
		status: missing.length > 0 ? 'failed' : 'succeeded',
		...(missing.length > 0 ? {
			failure_classification: 'provider',
			failure_code: 'agent_task.pi_required_outputs_missing',
		} : {}),
		summary: missing.length > 0
			? `Pi completed without required structured output(s): ${missing.join(', ')}.`
			: finalTextSummary(stream.finalText) || 'Pi completed the task.',
		...(structured.outputs ? { outputs: structured.outputs } : {}),
		diagnostics: [
			{ classification: 'provider', message: `Pi finished after ${stream.turns} turn(s) and ${stream.toolCalls} tool call(s).` },
			...(structured.diagnostics || []),
		],
		metadata: piMetadata(context, stream, { exit_code: 0 }),
	};
}

function failureFromStream(context) {
	const stream = parsePiEventStream(context.spawnResult.stdout);
	const status = context.spawnResult.status;
	const reason = stream.finalAssistant?.errorMessage || stream.retryFailure || stderrTail(context.spawnResult) || `Pi exited with status ${status}.`;
	return {
		status: 'failed',
		failure_classification: 'execution_failed',
		failure_code: 'agent_task.pi_failed',
		summary: `Pi execution failed: ${reason}`.slice(0, 500),
		diagnostics: [{ classification: 'execution_failed', message: `Pi exited with status ${status}. ${reason}`.trim() }],
		metadata: piMetadata(context, stream, {
			exit_code: status,
			...(context.spawnResult.signal ? { signal: context.spawnResult.signal } : {}),
		}),
	};
}

const { execute: executePiAgentTask, outcome, validationFailure } = createCliAgentTaskExecutor({
	backend: PI_BACKEND,
	runtime: PI_BACKEND,
	providerId: PI_PROVIDER_ID,
	providerLabel: 'Pi agent',
	defaultSummary: 'Pi agent task executor failed before producing a detailed outcome.',
	requireConfig: false,
	artifactProvider: 'pi',
	collectArtifacts: true,
	timeoutFallback: (config) => config.timeout_seconds || DEFAULT_TIMEOUT_SECONDS,
	resolveCommandSpec,
	buildArgs: (request, config, commandSpec) => {
		const model = requestedModel(request, config);
		return [
			...commandSpec.args,
			...sessionArgs(request, config),
			...(model ? ['--model', model] : []),
			promptArgument(request, config),
		];
	},
	buildSpawn: (request, config, options) => ({
		env: cliAgentTaskSpawnEnv(request, options, { allowlist: PI_ENV_ALLOWLIST }),
		// Pi treats piped stdin as extra prompt input; keep it empty.
		input: '',
	}),
	successOutcome: successFromStream,
	failureOutcome: failureFromStream,
	messages: {
		invalidRequest: { code: 'agent_task.invalid_pi_request', summary: 'Pi request validation failed.' },
		invalidCommand: { code: 'agent_task.invalid_pi_command', summary: 'Pi command configuration is invalid.' },
		notFound: { code: 'agent_task.pi_command_not_found', summary: 'Pi command was not found.', hint: 'Install Pi (npm install -g @earendil-works/pi-coding-agent) or configure executor.config.command.' },
		timeout: { code: 'agent_task.pi_timeout', summary: 'Pi execution timed out.' },
		spawnFailed: { code: 'agent_task.pi_spawn_failed', summary: 'Pi process failed to start or complete.' },
	},
});

module.exports = {
	MAX_INLINE_INSTRUCTIONS_BYTES,
	PI_BACKEND,
	PI_CAPABILITIES,
	PI_DEFAULT_COMMAND,
	PI_DEFAULT_COMMAND_ARGS,
	PI_PROVIDER_ID,
	PI_PROVIDER_LABEL,
	PI_READINESS_INVOCATION,
	executePiAgentTask,
	outcome,
	parsePiEventStream,
	piRuntimeReadiness,
	providerContract,
	validationFailure,
};
