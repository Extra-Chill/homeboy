'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { runWordPressFuzzRunnerResult } = require('../lib/wordpress-fuzz-runner');

async function main() {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-fuzz-nested-report-'));
	const escapedRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-fuzz-escaped-report-'));
	try {
	const workloadPath = path.join(root, 'workload.json');
	const reportPath = path.join(root, 'runtime', 'files', 'runtime-evidence', 'typed-artifacts', 'report.json');
	const escapedReportPath = path.join(escapedRoot, 'report.json');
	const escapedLinkPath = path.join(root, 'escaped-report.json');
	fs.mkdirSync(path.dirname(reportPath), { recursive: true });
	fs.writeFileSync(workloadPath, JSON.stringify({ id: 'nested-report', plan: { id: 'nested-plan', targets: [] } }));
	fs.writeFileSync(reportPath, JSON.stringify({
		schema: 'example/differential-report/v1',
		status: 'passed',
		cases: [
			{ schema: 'homeboy/fuzz-case/v1', id: 'case-a', target_id: 'target-a', operation_id: 'query.read', status: 'passed' },
			{ schema: 'homeboy/fuzz-case/v1', id: 'case-b', target_id: 'target-a', operation_id: 'dml.generic', status: 'skipped', metadata: { skip_reason: 'unsupported' } },
		],
		coverage_summary: { schema: 'homeboy/fuzz-coverage-summary/v1', declared_targets: 1, executable_targets: 1, proven_targets: 1, declared_operations: 2, executable_operations: 1, proven_operations: 1, skipped_targets: [], skipped_operations: [{ id: 'dml.generic', reason: 'unsupported' }] },
		homeboy_campaign: { id: 'nested-campaign', artifacts: [] },
	}));
	fs.writeFileSync(escapedReportPath, JSON.stringify({
		status: 'passed',
		cases: [{ schema: 'homeboy/fuzz-case/v1', id: 'escaped-case', status: 'passed' }],
		coverage_summary: { schema: 'homeboy/fuzz-coverage-summary/v1' },
	}));
	fs.symlinkSync(escapedReportPath, escapedLinkPath);

		const result = await runWordPressFuzzRunnerResult({
		env: { workloadPath, runId: 'outer-run', workloadId: 'nested-report', artifactRoot: root },
		runFuzzSuite: async () => ({
			schema: 'wp-codebox/fuzz-suite-result/v1',
			request_id: 'outer-run',
			status: 'passed',
			cases: [{ id: 'wrapper', status: 'passed' }],
			artifactRefs: [
				{ kind: 'typed-artifact', path: 'files/runtime-evidence/typed-artifacts/escaped-report.json', metadata: { artifactId: 'escaped_report', semantic_key: 'fuzz.report', sourcePath: escapedLinkPath } },
				{ kind: 'typed-artifact', path: 'files/runtime-evidence/typed-artifacts/report.json', metadata: { artifactId: 'mdi_native_sqlite_differential', semantic_key: 'fuzz.report', sourcePath: reportPath } },
			],
		}),
	});

		assert.deepEqual(result.wp_codebox_result.wordpress_fuzz_result.cases.map((entry) => entry.id), ['case-a', 'case-b']);
		assert.equal(result.wp_codebox_result.coverage_summary.proven_operations, 1);
		assert.equal(result.homeboy_fuzz_campaign.cases.length, 2);
		assert.equal(result.homeboy_fuzz_result_envelope.campaign.cases.length, 2);
		assert.equal(result.wp_codebox_result.artifacts.some((artifact) => artifact.metadata?.sourcePath), false);
		assert.equal(result.wp_codebox_result.artifacts.find((artifact) => artifact.name === 'mdi_native_sqlite_differential').path, path.relative(root, reportPath));
		assert.equal(result.homeboy_fuzz_campaign.artifacts.find((artifact) => artifact.id === 'case-log').artifact.path, 'files/case-log.jsonl');
	} finally {
		fs.rmSync(root, { recursive: true, force: true });
		fs.rmSync(escapedRoot, { recursive: true, force: true });
	}
}

main().then(() => console.log('wordpress fuzz nested report promotion smoke passed')).catch((error) => {
	console.error(error);
	process.exitCode = 1;
});
