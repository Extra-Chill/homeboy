#!/usr/bin/env node
'use strict';

const {
	parseJsonInput,
	writeGithubOutput,
} = require('./lib/common.cjs');
const {
	renderRuntimeWorkflowInputs,
} = require('../../../runtime-agent-ci/lib/runtime-workflow-inputs.cjs');

function main() {
	const rendered = renderRuntimeWorkflowInputs({
		runtime: process.env.RUNTIME || process.env.RUNTIME_PROVIDER || process.env.BACKEND,
		runtime_profile: parseRuntimeProfile(process.env.RUNTIME_PROFILE || process.env.PROFILE || ''),
		runtime_profiles: parseJsonInput('runtime_profiles', process.env.RUNTIME_PROFILES || '{}', 'object', {}),
		tool_profile: parseJsonInput('tool_profile', process.env.TOOL_PROFILE || process.env.TOOL_POLICY || '{}', 'object', {}),
	});
	writeGithubOutput({
		runtime: rendered.workflow_inputs.runtime || rendered.runtime_id,
		profile: rendered.workflow_inputs.profile || rendered.runtime_profile,
		runtime_profiles: JSON.stringify(rendered.workflow_inputs.runtime_profiles || rendered.runtime_profiles),
		workflow_inputs: JSON.stringify(rendered.workflow_inputs),
	});
	process.stdout.write(`${JSON.stringify(rendered, null, 2)}\n`);
}

function parseRuntimeProfile(value) {
	const raw = String(value || '').trim();
	if (raw.startsWith('{')) {
		return JSON.parse(raw);
	}
	return raw;
}

if (require.main === module) {
	try {
		main();
	} catch (error) {
		process.stderr.write(`${error.message}\n`);
		process.exit(1);
	}
}
