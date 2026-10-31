'use strict';

const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { AGENT_TASK_ARTIFACT_SCHEMA } = require('../../agent-task-contracts');

const DEFAULT_BUDGET = { maxBytes: 50 * 1024 * 1024, maxFiles: 1_000, maxNodes: 2_000, maxDepth: 32 };

function harvestDeclaredArtifacts({ request = {}, config = {}, cwd = '', artifactDir = '' } = {}) {
	const declarations = declaredArtifacts(request);
	if (declarations.length === 0) {
		return emptyResult();
	}
	// Declarations without a source path describe candidates; they are not files to capture.
	const sourcedDeclarations = declarations.filter((declaration) => declaration.path !== undefined && declaration.path !== null);
	if (sourcedDeclarations.length === 0) {
		return emptyResult();
	}
	const workspaceRoot = realDirectory(cwd);
	const root = schedulerArtifactRoot(artifactDir);
	if (!workspaceRoot || !root) {
		return captureFailure(sourcedDeclarations, !workspaceRoot ? 'workspace_root' : 'artifact_root', !workspaceRoot ? 'The declared artifact workspace root does not exist.' : 'The scheduler-owned artifact root must be an existing non-symlink directory.');
	}
	const result = emptyResult();
	const destinations = declarationDestinations(sourcedDeclarations, request.task_id);
	const prepared = [];
	for (const declaration of sourcedDeclarations) {
		if (!validDeclaredPath(declaration.path)) {
			if (declaration.required) {
				result.errors.push({ artifact: declaration.name, code: 'invalid_path', message: 'Required declared artifacts need an explicit workspace-relative path.' });
			} else {
				result.missing.optional.push({ name: declaration.name, path: '', required: false });
			}
			continue;
		}
		if (destinations.collisions.has(declaration)) {
			result.errors.push({ artifact: declaration.name, code: 'destination_collision', message: 'Declared artifact destination collides with another declaration.' });
			continue;
		}
		const source = resolveSource(workspaceRoot, declaration.path);
		if (source.error) {
			result.errors.push({ artifact: declaration.name, code: 'unsafe_path', message: source.error });
			continue;
		}
		if (!fs.existsSync(source.path)) {
			(result.missing[declaration.required ? 'required' : 'optional']).push({ name: declaration.name, path: declaration.path, required: declaration.required === true });
			continue;
		}
		try {
			prepared.push({ declaration, collected: collectSource(source.path, workspaceRoot, artifactBudget(request, config)) });
		} catch (error) {
			result.errors.push({ artifact: declaration.name, code: 'capture_failed', message: error.message });
		}
	}
	if (prepared.length === 0) {
		return result;
	}
	let stage;
	try {
		stage = privateArtifactStage(root);
	} catch (error) {
		return { ...result, errors: [...result.errors, ...prepared.map(({ declaration }) => ({ artifact: declaration.name, code: 'artifact_root', message: error.message }))] };
	}
	for (const { declaration, collected } of prepared) {
		try {
			const destination = path.join(stage.path, safeFileSegment(request.task_id), safeFileSegment(declaration.name));
			copyStagedEntries(stage, destination, collected.entries);
			verifyRetainedPath(root, stage, destination);
			const digest = collected.directory ? digestTree(collected.entries) : collected.entries[0].digest;
			const uri = pathToFileURL(destination).href;
			const artifact = {
				schema: AGENT_TASK_ARTIFACT_SCHEMA,
				id: declaration.id || declaration.name,
				name: declaration.name,
				kind: declaration.kind || declaration.artifact_type || declaration.type || (collected.directory ? 'directory' : 'file'),
				...(declaration.artifact_type ? { artifact_type: declaration.artifact_type } : {}),
				...(declaration.artifact_schema || declaration.schema ? { artifact_schema: declaration.artifact_schema || declaration.schema } : {}),
				path: destination,
				uri,
				url: uri,
				required: declaration.required === true,
				bytes: collected.bytes,
				size_bytes: collected.bytes,
				sha256: digest,
				file_count: collected.entries.filter((entry) => !entry.directory).length,
				node_count: collected.entries.length,
				provenance: { source_path: declaration.path, workspace_root: workspaceRoot },
				...(declaration.description ? { description: declaration.description } : {}),
				...(plainObject(declaration.metadata) ? { metadata: declaration.metadata } : {}),
			};
			result.artifacts.push(artifact);
			result.evidence_refs.push({ kind: artifact.kind, label: artifact.name, uri, sha256: digest });
		} catch (error) {
			result.errors.push({ artifact: declaration.name, code: 'capture_failed', message: error.message });
		}
	}
	return result;
}

function declaredArtifacts(request = {}) {
	const source = Array.isArray(request.artifact_declarations) ? request.artifact_declarations : (Array.isArray(request.executor?.artifact_declarations) ? request.executor.artifact_declarations : []);
	return source.filter((artifact) => plainObject(artifact) && (artifact.name || artifact.id)).map((artifact) => ({ ...artifact, name: String(artifact.name || artifact.id) }));
}

function declarationDestinations(declarations, taskId) {
	const keys = new Map();
	const counts = new Map();
	for (const declaration of declarations) {
		const key = `${safeFileSegment(taskId)}/${safeFileSegment(declaration.name)}`;
		keys.set(declaration, key);
		counts.set(key, (counts.get(key) || 0) + 1);
	}
	return { collisions: new Set(declarations.filter((declaration) => counts.get(keys.get(declaration)) > 1)) };
}

function collectSource(source, workspaceRoot, budget) {
	const entries = [];
	let bytes = 0;
	const visit = (current, relative, depth) => {
		if (depth > budget.maxDepth) {
			throw new Error(`Declared artifact exceeds the ${budget.maxDepth}-level traversal budget.`);
		}
		if (entries.length >= budget.maxNodes) {
			throw new Error(`Declared artifact exceeds the ${budget.maxNodes}-node budget.`);
		}
		const stat = fs.lstatSync(current);
		if (stat.isSymbolicLink() || !stat.isFile() && !stat.isDirectory()) {
			throw new Error('Declared artifact tree contains a symlink or unsafe file type.');
		}
		const real = fs.realpathSync(current);
		if (!isWithin(workspaceRoot, real)) {
			throw new Error('Declared artifact source resolves outside the workspace root.');
		}
		if (stat.isDirectory()) {
			verifyDirectoryDescriptor(current, workspaceRoot);
			entries.push({ relative, directory: true });
			for (const name of fs.readdirSync(current).sort()) {
				visit(path.join(current, name), path.join(relative, name), depth + 1);
			}
			return;
		}
		if (entries.filter((entry) => !entry.directory).length >= budget.maxFiles) {
			throw new Error(`Declared artifact exceeds the ${budget.maxFiles}-file budget.`);
		}
		const staged = stagedFile(current, budget.maxBytes - bytes, workspaceRoot);
		bytes += staged.bytes;
		entries.push({ relative, directory: false, ...staged });
	};
	visit(source, '', 0);
	return { entries, bytes, directory: entries[0].directory };
}

function copyStagedEntries(stage, destination, entries) {
	for (const entry of entries) {
		const target = entry.relative ? path.join(destination, entry.relative) : destination;
		assertStagePath(stage, target);
		if (entry.directory) {
			fs.mkdirSync(target, { recursive: true, mode: 0o700 });
			continue;
		}
		fs.mkdirSync(path.dirname(target), { recursive: true, mode: 0o700 });
		writeNewFile(target, entry.content);
	}
}

function privateArtifactStage(root) {
	assertRoot(root);
	const stagePath = fs.mkdtempSync(path.join(root.path, '.homeboy-declared-'));
	fs.chmodSync(stagePath, 0o700);
	const stat = fs.lstatSync(stagePath);
	const real = fs.realpathSync(stagePath);
	if (stat.isSymbolicLink() || !stat.isDirectory() || !isWithin(root.path, real)) {
		throw new Error('Could not create a private scheduler artifact staging directory.');
	}
	return { path: real, dev: stat.dev, ino: stat.ino };
}

function verifyRetainedPath(root, stage, target) {
	assertRoot(root);
	const stageStat = fs.lstatSync(stage.path);
	if (stageStat.isSymbolicLink() || stageStat.dev !== stage.dev || stageStat.ino !== stage.ino) {
		throw new Error('Private scheduler artifact staging directory changed during capture.');
	}
	const real = fs.realpathSync(target);
	if (!isWithin(stage.path, real) || !isWithin(root.path, real)) {
		throw new Error('Retained artifact path escapes the private scheduler artifact root.');
	}
}

function schedulerArtifactRoot(value) {
	if (!value) {
		return null;
	}
	try {
		const input = path.resolve(value);
		const stat = fs.lstatSync(input);
		const real = fs.realpathSync(input);
		return !stat.isSymbolicLink() && stat.isDirectory() ? { path: real, dev: stat.dev, ino: stat.ino } : null;
	} catch {
		return null;
	}
}

function assertRoot(root) {
	const stat = fs.lstatSync(root.path);
	if (stat.isSymbolicLink() || !stat.isDirectory() || stat.dev !== root.dev || stat.ino !== root.ino || fs.realpathSync(root.path) !== root.path) {
		throw new Error('Scheduler artifact root changed during capture.');
	}
}

function resolveSource(workspaceRoot, declaredPath) {
	if (path.isAbsolute(declaredPath) || declaredPath.includes('\0') || declaredPath.split(/[\\/]+/).includes('..')) {
		return { error: 'Declared artifact paths must be workspace-relative.' };
	}
	const candidate = path.resolve(workspaceRoot, declaredPath);
	return isWithin(workspaceRoot, candidate) ? { path: candidate } : { error: 'Declared artifact path escapes the workspace root.' };
}

function stagedFile(filePath, maxBytes, workspaceRoot) {
	const fd = fs.openSync(filePath, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
	try {
		const stat = fs.fstatSync(fd);
		if (!stat.isFile() || stat.size > maxBytes) {
			throw new Error(stat.isFile() ? 'Declared artifact exceeds the byte budget.' : 'Declared artifact source must be a regular file.');
		}
		verifyDescriptorPath(fd, workspaceRoot);
		const content = Buffer.alloc(stat.size);
		for (let offset = 0; offset < content.length;) {
			const count = fs.readSync(fd, content, offset, content.length - offset, offset);
			if (count === 0) {
				throw new Error('Declared artifact changed while it was being staged.');
			}
			offset += count;
		}
		return { bytes: stat.size, content, digest: crypto.createHash('sha256').update(content).digest('hex') };
	} finally {
		fs.closeSync(fd);
	}
}

function verifyDirectoryDescriptor(directory, workspaceRoot) {
	const fd = fs.openSync(directory, fs.constants.O_RDONLY | fs.constants.O_DIRECTORY | fs.constants.O_NOFOLLOW);
	try {
		if (!fs.fstatSync(fd).isDirectory()) {
			throw new Error('Declared artifact source must be a directory.');
		}
		verifyDescriptorPath(fd, workspaceRoot);
	} finally {
		fs.closeSync(fd);
	}
}

function verifyDescriptorPath(fd, workspaceRoot) {
	const descriptorPath = openedDescriptorPath(fd);
	if (!descriptorPath || !isWithin(workspaceRoot, descriptorPath)) {
		throw new Error('Declared artifact descriptor resolves outside the workspace root.');
	}
}

function openedDescriptorPath(fd) {
	for (const base of ['/proc/self/fd', '/dev/fd']) {
		try {
			const resolved = fs.realpathSync(path.join(base, String(fd)));
			if (path.isAbsolute(resolved) && !resolved.startsWith(`${base}/`)) {
				return resolved;
			}
		} catch {
			// Try the next platform descriptor resolver.
		}
	}
	const result = spawnSync('lsof', ['-Fn', '-a', '-p', String(process.pid), '-d', String(fd)], { encoding: 'utf8' });
	const line = String(result.stdout || '').split('\n').find((value) => value.startsWith('n'));
	if (!line || !path.isAbsolute(line.slice(1))) {
		throw new Error('Descriptor identity proof is unavailable; refusing declared artifact source capture.');
	}
	try {
		return fs.realpathSync(line.slice(1));
	} catch {
		throw new Error('Descriptor identity proof could not resolve the opened source path.');
	}
}

function writeNewFile(target, content) {
	const fd = fs.openSync(target, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
	try {
		for (let offset = 0; offset < content.length;) {
			offset += fs.writeSync(fd, content, offset, content.length - offset, offset);
		}
		fs.fsyncSync(fd);
	} finally {
		fs.closeSync(fd);
	}
}

function digestTree(entries) {
	const digest = crypto.createHash('sha256');
	for (const entry of entries) {
		digest.update(`${entry.directory ? 'D' : 'F'}\0${entry.relative}\0${entry.digest || ''}\n`);
	}
	return digest.digest('hex');
}

function artifactBudget(request, config) {
	const limits = request.limits || {};
	return {
		maxBytes: positiveInteger(config.declared_artifact_max_bytes || config.declaredArtifactMaxBytes || limits.declared_artifact_max_bytes || limits.declaredArtifactMaxBytes, DEFAULT_BUDGET.maxBytes),
		maxFiles: positiveInteger(config.declared_artifact_max_files || config.declaredArtifactMaxFiles || limits.declared_artifact_max_files || limits.declaredArtifactMaxFiles, DEFAULT_BUDGET.maxFiles),
		maxNodes: positiveInteger(config.declared_artifact_max_nodes || config.declaredArtifactMaxNodes || limits.declared_artifact_max_nodes || limits.declaredArtifactMaxNodes, DEFAULT_BUDGET.maxNodes),
		maxDepth: positiveInteger(config.declared_artifact_max_depth || config.declaredArtifactMaxDepth || limits.declared_artifact_max_depth || limits.declaredArtifactMaxDepth, DEFAULT_BUDGET.maxDepth),
	};
}

function assertStagePath(stage, target) {
	if (!isWithin(stage.path, target)) {
		throw new Error('Artifact destination escapes the private scheduler artifact root.');
	}
}

function isWithin(root, target) {
	return target === root || target.startsWith(`${root}${path.sep}`);
}

function realDirectory(value) {
	try {
		return fs.statSync(value).isDirectory() ? fs.realpathSync(value) : '';
	} catch {
		return '';
	}
}

function validDeclaredPath(value) { return typeof value === 'string' && value !== ''; }
function positiveInteger(value, fallback) { return Number.isSafeInteger(Number(value)) && Number(value) > 0 ? Number(value) : fallback; }
function safeFileSegment(value) { return String(value || 'artifact').replace(/[^a-z0-9._-]+/gi, '-').replace(/^-+|-+$/g, '') || 'artifact'; }
function plainObject(value) { return value && typeof value === 'object' && !Array.isArray(value); }
function emptyResult() { return { artifacts: [], evidence_refs: [], errors: [], missing: { required: [], optional: [] } }; }
function captureFailure(declarations, code, message) { const result = emptyResult(); result.errors = declarations.map((declaration) => ({ artifact: declaration.name, code, message })); return result; }

module.exports = { harvestDeclaredArtifacts };
