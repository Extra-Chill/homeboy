import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(dirname, '..');
const manifestPaths = [
	'wordpress/wordpress.json',
	'nodejs/nodejs.json',
	'rust/rust.json',
	'swift/swift.json',
	'go/go.json',
];

const requiredKeys = [
	'lint.findings',
	'test.results',
	'test.failures',
	'test.coverage',
	'annotations',
];

const expectedSupport = {
	'wordpress/wordpress.json': {
		'lint.findings': true,
		'lint.producers': true,
		'test.results': true,
		'test.failures': true,
		'test.coverage': false,
		'bench.results': true,
		'trace.results': true,
		annotations: false,
	},
	'nodejs/nodejs.json': {
		'lint.findings': true,
		'test.results': true,
		'test.failures': true,
		'test.coverage': false,
		'bench.results': true,
		'trace.results': true,
		'trace.artifacts': true,
		annotations: false,
	},
	'rust/rust.json': {
		'lint.findings': true,
		'test.results': true,
		'test.failures': true,
		'test.coverage': false,
		'bench.results': true,
		annotations: true,
	},
	'swift/swift.json': {
		'lint.findings': true,
		'test.results': false,
		'test.failures': true,
		'test.coverage': false,
		annotations: false,
	},
	'go/go.json': {
		'lint.findings': true,
		'test.results': false,
		'test.failures': true,
		'test.coverage': false,
		annotations: false,
	},
};

const supportEvidence = {
	'wordpress/wordpress.json': {
		'lint.findings': ['wordpress/scripts/lint/lint-runner.sh', 'HOMEBOY_LINT_FINDINGS_FILE'],
		'lint.producers': ['wordpress/scripts/lint/lint-runner.sh', 'HOMEBOY_LINT_PRODUCERS_FILE'],
		'test.results': ['wordpress/scripts/test/test-runner-wp-codebox.sh', 'HOMEBOY_TEST_RESULTS_FILE'],
		'test.failures': ['wordpress/scripts/test/test-runner-wp-codebox.sh', 'HOMEBOY_TEST_FAILURES_FILE'],
		'bench.results': ['wordpress/scripts/bench/bench-runner-wp-codebox.sh', 'HOMEBOY_BENCH_RESULTS_FILE'],
		'trace.results': ['wordpress/scripts/trace/trace-runner.sh', 'HOMEBOY_TRACE_RESULTS_FILE'],
	},
	'nodejs/nodejs.json': {
		'lint.findings': ['nodejs/scripts/lint/lint-runner.sh', 'HOMEBOY_LINT_FINDINGS_FILE'],
		'test.results': ['nodejs/scripts/test/test-runner.sh', 'HOMEBOY_TEST_RESULTS_FILE'],
		'test.failures': ['nodejs/scripts/test/test-runner.sh', 'HOMEBOY_TEST_FAILURES_FILE'],
		'bench.results': ['nodejs/scripts/bench/bench-runner.sh', 'HOMEBOY_BENCH_RESULTS_FILE'],
		'trace.results': ['nodejs/scripts/trace/trace-runner.sh', 'HOMEBOY_TRACE_RESULTS_FILE'],
		'trace.artifacts': ['nodejs/scripts/trace/trace-runner.sh', 'HOMEBOY_TRACE_ARTIFACT_DIR'],
	},
	'rust/rust.json': {
		'lint.findings': ['rust/scripts/lint-runner.sh', 'HOMEBOY_LINT_FINDINGS_FILE'],
		'test.results': ['rust/scripts/test-runner.sh', 'HOMEBOY_TEST_RESULTS_FILE'],
		'test.failures': ['rust/scripts/test-runner.sh', 'HOMEBOY_TEST_FAILURES_FILE'],
		'bench.results': ['rust/scripts/bench/bench-runner.sh', 'HOMEBOY_BENCH_RESULTS_FILE'],
		annotations: ['rust/scripts/lint-runner.sh', 'HOMEBOY_ANNOTATIONS_DIR'],
	},
	'swift/swift.json': {
		'lint.findings': ['swift/scripts/lint-runner.sh', 'HOMEBOY_LINT_FINDINGS_FILE'],
		'test.failures': ['swift/scripts/test-runner.sh', 'HOMEBOY_TEST_FAILURES_FILE'],
	},
	'go/go.json': {
		'lint.findings': ['go/scripts/lint-runner.sh', 'HOMEBOY_LINT_FINDINGS_FILE'],
		'test.failures': ['go/scripts/test-runner.sh', 'HOMEBOY_TEST_FAILURES_FILE'],
	},
};

for (const manifestPath of manifestPaths) {
	const manifest = JSON.parse(await readFile(path.join(root, manifestPath), 'utf8'));
	assert.equal(typeof manifest.structured_sidecars, 'object', `${manifestPath} declares structured_sidecars`);
	assert.equal(Array.isArray(manifest.structured_sidecars), false, `${manifestPath} sidecars must be an object`);
	assert.equal(manifest.audit?.test_mapping, undefined, `${manifestPath} keeps structural test coverage out of audit findings`);

	if (manifestPath !== 'nodejs/nodejs.json') {
		assert.equal(typeof manifest.test?.drift, 'object', `${manifestPath} keeps changed-test drift guidance configured`);
	}

	for (const key of requiredKeys) {
		assert.equal(typeof manifest.structured_sidecars[key], 'boolean', `${manifestPath} declares ${key}`);
	}

	assert.deepEqual(manifest.structured_sidecars, expectedSupport[manifestPath], `${manifestPath} sidecar support matches audited runner contracts`);

	for (const [key, evidence] of Object.entries(supportEvidence[manifestPath] || {})) {
		assert.equal(manifest.structured_sidecars[key], true, `${manifestPath} declares ${key} support`);
		const [evidencePath, needle] = evidence;
		const source = await readFile(path.join(root, evidencePath), 'utf8');
		assert.match(source, new RegExp(needle), `${manifestPath} ${key} is backed by ${needle} in ${evidencePath}`);
	}
}
