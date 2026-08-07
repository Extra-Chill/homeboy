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
fs.mkdirSync(path.join(workspace, 'screenshots', 'empty'));
fs.mkdirSync(artifacts);

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
	assert.equal(report.schema, 'homeboy/agent-task-artifact/v1');
	assert.equal(new URL(report.uri).protocol, 'file:');
	assert.equal(report.url, report.uri);
	assert.equal(report.size_bytes, Buffer.byteLength('# Report\n'));
	assert.equal(report.sha256, crypto.createHash('sha256').update('# Report\n').digest('hex'));
	assert.deepEqual(report.metadata, { source: 'agent' });
	assert.equal(fs.readFileSync(report.path, 'utf8'), '# Report\n');
	fs.writeFileSync(path.join(workspace, 'report.md'), 'changed after staging\n');
	assert.equal(fs.readFileSync(report.path, 'utf8'), '# Report\n');
	assert.equal(screenshots.file_count, 2);
	assert.equal(screenshots.node_count, 4);
	assert.deepEqual(fs.readFileSync(path.join(screenshots.path, 'image.bin')), binary);
	assert.equal(fs.statSync(path.join(screenshots.path, 'empty')).isDirectory(), true);
	assert.match(screenshots.path, /\.homeboy-declared-/);

	const rejected = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'escape', path: '../outside.txt', required: true }] },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.equal(rejected.errors[0].code, 'unsafe_path');
	assert.equal(rejected.artifacts.length, 0);
	const missingPath = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'required-path', required: true }] },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.deepEqual(missingPath, { artifacts: [], evidence_refs: [], errors: [], missing: { required: [], optional: [] } });
	const pathlessWithoutArtifactRoot = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'candidate', required: true }] },
		cwd: workspace,
	});
	assert.deepEqual(pathlessWithoutArtifactRoot, { artifacts: [], evidence_refs: [], errors: [], missing: { required: [], optional: [] } });
	const emptyPath = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'empty-path', path: '', required: true }] },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.equal(emptyPath.errors[0].code, 'invalid_path');
	const collision = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'same/name', path: 'report.md' }, { name: 'same-name', path: 'report.md' }] },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.deepEqual(collision.errors.map((error) => error.code), ['destination_collision', 'destination_collision']);
	assert.equal(collision.artifacts.length, 0);
	const nodeBudget = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'too-many-nodes', path: 'screenshots' }] },
		config: { declared_artifact_max_nodes: 3 },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.equal(nodeBudget.errors[0].code, 'capture_failed');
	fs.rmdirSync(path.join(workspace, 'screenshots', 'empty'));
	const withoutEmptyDirectory = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'screenshots-without-empty', path: 'screenshots' }] },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.notEqual(withoutEmptyDirectory.artifacts[0].sha256, screenshots.sha256);

	fs.writeFileSync(path.join(root, 'outside.txt'), 'outside workspace\n');
	fs.symlinkSync(path.join(root, 'outside.txt'), path.join(workspace, 'escaped-link'));
	const symlink = harvestDeclaredArtifacts({
		request: { artifact_declarations: [{ name: 'link', path: 'escaped-link', required: true }] },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.equal(symlink.errors[0].code, 'capture_failed');

	const swappedSourceDirectory = path.join(workspace, 'swap-source');
	const outsideDirectory = path.join(root, 'outside-source');
	const swappedSourceFile = path.join(fs.realpathSync(workspace), 'swap-source', 'report.md');
	fs.mkdirSync(swappedSourceDirectory);
	fs.mkdirSync(outsideDirectory);
	fs.writeFileSync(swappedSourceFile, 'inside workspace\n');
	fs.writeFileSync(path.join(outsideDirectory, 'report.md'), 'outside content\n');
	const originalOpen = fs.openSync;
	try {
		fs.openSync = (filePath, flags, ...args) => {
			if (filePath === swappedSourceFile && typeof flags === 'number') {
				fs.rmSync(swappedSourceDirectory, { recursive: true, force: true });
				fs.symlinkSync(outsideDirectory, swappedSourceDirectory);
			}
			return originalOpen(filePath, flags, ...args);
		};
		const ancestorSwap = harvestDeclaredArtifacts({
			request: { task_id: 'source-ancestor-swap', artifact_declarations: [{ name: 'report', path: 'swap-source/report.md', required: true }] },
			cwd: workspace,
			artifactDir: artifacts,
		});
		assert.equal(ancestorSwap.errors[0].code, 'capture_failed');
		assert.equal(ancestorSwap.artifacts.length, 0);
		assert.equal(fs.readFileSync(path.join(outsideDirectory, 'report.md'), 'utf8'), 'outside content\n');
	} finally {
		fs.openSync = originalOpen;
	}

	const destination = path.join(artifacts, 'declared', 'destination-symlink', 'report');
	fs.mkdirSync(path.dirname(destination), { recursive: true });
	const protectedFile = path.join(root, 'protected-final.txt');
	fs.writeFileSync(protectedFile, 'protected\n');
	fs.symlinkSync(protectedFile, destination);
	const finalSymlink = harvestDeclaredArtifacts({
		request: { task_id: 'destination-symlink', artifact_declarations: [{ name: 'report', path: 'report.md' }] },
		cwd: workspace,
		artifactDir: artifacts,
	});
	assert.equal(finalSymlink.errors.length, 0);
	assert.equal(fs.readFileSync(protectedFile, 'utf8'), 'protected\n');

	const ancestorRoot = path.join(root, 'ancestor-artifacts');
	const protectedDirectory = path.join(root, 'protected-directory');
	fs.mkdirSync(ancestorRoot);
	fs.mkdirSync(protectedDirectory);
	fs.symlinkSync(protectedDirectory, path.join(ancestorRoot, 'declared'));
	const ancestorSymlink = harvestDeclaredArtifacts({
		request: { task_id: 'ancestor-symlink', artifact_declarations: [{ name: 'report', path: 'report.md' }] },
		cwd: workspace,
		artifactDir: ancestorRoot,
	});
	assert.equal(ancestorSymlink.errors.length, 0);
	assert.equal(fs.readdirSync(protectedDirectory).length, 0);

	const swapRoot = path.join(root, 'swap-artifacts');
	const swapTarget = path.join(root, 'swap-target');
	fs.mkdirSync(swapRoot);
	fs.mkdirSync(swapTarget);
	const realSwapRoot = fs.realpathSync(swapRoot);
	const originalMkdtemp = fs.mkdtempSync;
	try {
		fs.mkdtempSync = (prefix, options) => {
			const result = originalMkdtemp(prefix, options);
			if (prefix.startsWith(realSwapRoot)) {
				fs.rmSync(result, { recursive: true, force: true });
				fs.symlinkSync(swapTarget, result);
			}
			return result;
		};
		const swap = harvestDeclaredArtifacts({
			request: { task_id: 'swap-attempt', artifact_declarations: [{ name: 'report', path: 'report.md' }] },
			cwd: workspace,
			artifactDir: swapRoot,
		});
		assert.equal(swap.errors[0].code, 'artifact_root');
		assert.equal(fs.readdirSync(swapTarget).length, 0);
	} finally {
		fs.mkdtempSync = originalMkdtemp;
	}
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('Declared artifact harvester passed\n');
