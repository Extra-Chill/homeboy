'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { harvestDeclaredArtifacts } = require('../../lib/declared-artifact-harvester');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-declared-artifacts-'));
const workspace = path.join(root, 'workspace');
const artifacts = path.join(root, 'scheduler-artifacts');
fs.mkdirSync(path.join(workspace, 'screenshots'), { recursive: true });
fs.writeFileSync(path.join(workspace, 'report.md'), '# Report\n');
const binary = Buffer.from([0, 255, 1, 254, 2]);
fs.writeFileSync(path.join(workspace, 'screenshots', 'image.bin'), binary);
fs.writeFileSync(path.join(workspace, 'screenshots', 'caption.txt'), 'Screenshot caption\n');

try {
	const result = harvestDeclaredArtifacts({
		request: {
			task_id: 'declared-artifacts',
			artifact_declarations: [
				{ name: 'report', path: 'report.md', kind: 'markdown', artifact_type: 'report', artifact_schema: 'example/report/v1', required: true, metadata: { source: 'agent' } },
				{ name: 'screenshots', path: 'screenshots', kind: 'screenshot-directory', required: true },
				{ name: 'optional-video', path: 'video.webm', required: false },
			],
		},
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.deepEqual(result.errors, []);
	assert.deepEqual(result.missing.required, []);
	assert.deepEqual(result.missing.optional, [{ name: 'optional-video', path: 'video.webm', required: false }]);
	assert.equal(result.artifacts.length, 2);
	const report = result.artifacts.find((artifact) => artifact.name === 'report');
	const screenshots = result.artifacts.find((artifact) => artifact.name === 'screenshots');
	assert.equal(report.artifact_schema, 'example/report/v1');
	assert.equal(report.artifact_type, 'report');
	assert.equal(report.required, true);
	assert.equal(report.sha256, crypto.createHash('sha256').update('# Report\n').digest('hex'));
	assert.deepEqual(report.metadata, { source: 'agent' });
	assert.equal(fs.readFileSync(report.path, 'utf8'), '# Report\n');
	assert.equal(screenshots.file_count, 2);
	assert.deepEqual(fs.readFileSync(path.join(screenshots.path, 'image.bin')), binary);
	assert.equal(screenshots.sha256, '831c9f2aacaee843fd4a84b4ae13732902470e83baed13fc52fb08c901cbde6f');

	const rejected = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'escape', path: '../outside.txt', required: true }] },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.equal(rejected.errors[0].code, 'unsafe_path');
	assert.equal(rejected.artifacts.length, 0);

	fs.writeFileSync(path.join(root, 'outside.txt'), 'outside workspace\n');
	fs.symlinkSync(path.join(root, 'outside.txt'), path.join(workspace, 'escaped-link'));
	const symlink = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'link', path: 'escaped-link', required: true }] },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.equal(symlink.errors[0].code, 'capture_failed');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('Declared artifact harvester passed\n');
