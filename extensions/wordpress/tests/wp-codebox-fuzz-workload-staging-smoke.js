const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { wpCodeboxFuzzSuiteInput } = require('../lib/wp-codebox-fuzz-run');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-fuzz-workload-staging-'));
const bench = path.join(root, 'bench');
fs.mkdirSync(bench, { recursive: true });

const phpWorkloadPath = path.join(bench, 'rest-product-batch-import.php');
const jsonWorkloadPath = path.join(bench, 'rest-product-batch-import.workload.json');
const sandboxPhpWorkloadPath = '/tmp/homeboy-wp-codebox-workloads/bench/rest-product-batch-import.php';
const executionRequest = {
	schema: 'homeboy/fuzz-execution-request/v1',
	id: 'nested-workload-request',
	component: 'woocommerce',
	metadata: { planner: { profile: 'quick' } },
};

fs.writeFileSync(phpWorkloadPath, '<?php return function (): array { return array("status" => "passed"); };\n', 'utf8');
fs.writeFileSync(jsonWorkloadPath, `${JSON.stringify({
	schema: 'wp-codebox/wordpress-workload-run/v1',
	run: [{ command: 'wordpress.run-workload', args: [`path=${phpWorkloadPath}`, 'type=php'] }],
	artifacts: [{ name: 'nested-report', path: 'files/workload-results/nested-report.json', kind: 'json', required: true, metadata: { semantic_key: 'fuzz.report' } }],
})}\n`, 'utf8');

for (const pathRef of ['metadata.workload_path', 'workload.path', 'intent.execute.path']) {
	const manifest = {
		schema: 'homeboy/fuzz-workload/v1',
		id: `rest-product-batch-import-${pathRef}`,
		workload: {},
		metadata: {},
		cases: [{ id: 'default', intent: { execute: { type: 'json' } } }],
	};

	if (pathRef === 'metadata.workload_path') {
		manifest.metadata.workload_path = jsonWorkloadPath;
	}
	if (pathRef === 'workload.path') {
		manifest.workload.path = jsonWorkloadPath;
	}
	if (pathRef === 'intent.execute.path') {
		manifest.cases[0].intent.execute.path = jsonWorkloadPath;
	}

	const input = wpCodeboxFuzzSuiteInput({ id: `run-${pathRef}`, homeboyFuzzWorkload: manifest, executionRequest });
	assert.deepEqual(input.execution_request, executionRequest);
	assert.deepEqual(input.cases[0].input.execution_request, executionRequest);
	assert.deepEqual(input.cases[0].input.staged_files, [{ source: phpWorkloadPath, target: sandboxPhpWorkloadPath }]);
	assert.deepEqual(input.cases[0].input.steps, [{ command: 'wordpress.run-workload', args: [`path=${sandboxPhpWorkloadPath}`, 'type=php'] }]);
	assert.deepEqual(input.cases[0].input.artifacts, [{ name: 'nested-report', path: 'files/workload-results/nested-report.json', kind: 'json', required: true, metadata: { semantic_key: 'fuzz.report' } }]);
}

fs.rmSync(root, { recursive: true, force: true });
