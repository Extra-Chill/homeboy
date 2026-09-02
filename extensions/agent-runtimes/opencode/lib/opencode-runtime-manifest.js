'use strict';

const {
	providerContract,
} = require('./opencode-agent-task-executor');

function runtimeManifest() {
	return {
		schema: 'homeboy/agent-runtime-manifest/v1',
		id: 'opencode',
		name: 'OpenCode',
		version: '1.4.7',
		description: 'OpenCode agent runtime for nested orchestration and repository-scoped agent tasks.',
		requires: {
			// Older cores safely ignore the optional top-level retention capability.
			// Cleanup requires a core that implements external_storage_retention.
			homeboy: '>=0.345.0',
		},
		compatibility: {
			immediate_failure_patterns: {
				owner: 'Extra-Chill/homeboy#12293',
				requirement: 'Homeboy core support for agent-task executor immediate_failure_patterns.',
			},
		},
		contract_producers: [
			{
				id: 'opencode.agent-task-result',
				phase: 'result',
				invocation: {
					script: 'scripts/agent/homeboy-opencode-agent-task-executor.cjs',
					input_schema: 'homeboy/agent-task-request/v1',
					output_schema: 'homeboy/agent-task-outcome/v1',
				},
				produces: [
					{
						kind: 'status',
						name: 'agent-task-outcome',
						schema: 'homeboy/agent-task-outcome/v1',
					},
				],
			},
		],
		agent_task_executors: [providerContract()],
	};
}

module.exports = {
	runtimeManifest,
};
