'use strict';

const {
	providerContract,
} = require('./opencode-agent-task-executor');

function runtimeManifest() {
	return {
		schema: 'homeboy/agent-runtime-manifest/v1',
		id: 'opencode',
		name: 'OpenCode',
		version: '1.1.1',
		description: 'OpenCode agent runtime for nested orchestration and repository-scoped agent tasks.',
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
