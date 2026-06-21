'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const fsp = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');
const { wpCodeboxPluginStateStep } = require('./wp-codebox-recipe-helper');

function normalizeFixtureList(fixtures) {
	if (fixtures === undefined || fixtures === null) {
		return [];
	}
	if (!Array.isArray(fixtures)) {
		throw new TypeError('fixtures must be an array');
	}
	return fixtures.map((fixture, index) => normalizeFixtureStep(fixture, index));
}

function normalizeFixtureProfileSiteSeeds(profile) {
	if (profile === undefined || profile === null || profile === false) {
		return [];
	}
	if (Array.isArray(profile)) {
		return profile.map((siteSeed, index) => normalizeFixtureProfileSiteSeed(siteSeed, index));
	}
	if (!isPlainObject(profile)) {
		throw new TypeError('fixture profile must be an object or array');
	}
	if (Array.isArray(profile.siteSeeds)) {
		return profile.siteSeeds.map((siteSeed, index) => normalizeFixtureProfileSiteSeed(siteSeed, index));
	}
	return [normalizeFixtureProfileSiteSeed(profile, 0)];
}

function normalizeFixtureProfileSiteSeed(siteSeed, index) {
	if (!isPlainObject(siteSeed)) {
		throw new TypeError(`fixture profile site seed ${index + 1} must be an object`);
	}
	const scopes = siteSeed.scopes || pickSiteSeedScopes(siteSeed);
	if (!isPlainObject(scopes) || Object.keys(scopes).length === 0) {
		throw new TypeError(`fixture profile site seed ${index + 1} requires scopes`);
	}
	const name = siteSeed.name || siteSeed.id || `fixture-profile-${index + 1}`;
	if (typeof name !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(name)) {
		throw new TypeError(`fixture profile site seed ${index + 1} requires a valid name`);
	}
	const type = siteSeed.type || (siteSeed.source ? 'fixture' : 'parent_site');
	if (type !== 'fixture' && type !== 'parent_site') {
		throw new Error(`Unsupported fixture profile site seed type: ${type}`);
	}
	if (type === 'fixture' && (typeof siteSeed.source !== 'string' || siteSeed.source.trim() === '')) {
		throw new TypeError(`fixture profile site seed ${index + 1} requires source`);
	}

	return {
		type,
		name,
		...(siteSeed.source ? { source: siteSeed.source } : {}),
		...(siteSeed.format ? { format: siteSeed.format } : {}),
		...(siteSeed.deterministicIds ? { deterministicIds: siteSeed.deterministicIds } : {}),
		...(siteSeed.bootstrap ? { bootstrap: siteSeed.bootstrap } : {}),
		scopes,
	};
}

function pickSiteSeedScopes(siteSeed) {
	const scopes = {};
	for (const key of ['posts', 'terms', 'options', 'users', 'media', 'activePlugins', 'activeTheme']) {
		if (siteSeed[key] !== undefined) {
			scopes[key] = siteSeed[key];
		}
	}
	return scopes;
}

function normalizeFixtureStep(fixture, index) {
	if (!isPlainObject(fixture)) {
		throw new TypeError(`fixture step ${index + 1} must be an object`);
	}
	const type = fixture.type || (fixture.path ? 'wp-eval-file' : 'wp-cli');
	if (type !== 'wp-eval-file' && type !== 'wp-cli') {
		throw new Error(`Unsupported WordPress fixture step type: ${type}`);
	}

	if (type === 'wp-eval-file' && (typeof fixture.path !== 'string' || fixture.path.trim() === '')) {
		throw new TypeError(`fixture step ${index + 1} requires a non-empty path`);
	}
	if (type === 'wp-cli' && (typeof fixture.command !== 'string' || fixture.command.trim() === '')) {
		throw new TypeError(`fixture step ${index + 1} requires a non-empty command`);
	}

	return {
		...fixture,
		type,
		label: fixture.label || fixture.id || `${type}:${index + 1}`,
	};
}

function quoteShell(value) {
	return `'${String(value).replace(/'/g, `'\\''`)}'`;
}

function normalizeCliCommand(command) {
	const trimmed = String(command || '').trim();
	return trimmed.startsWith('wp ') ? trimmed.slice(3).trim() : trimmed;
}

function fixtureCommand(step) {
	if (step.type === 'wp-eval-file') {
		return `eval-file ${quoteShell(step.path)}`;
	}
	return normalizeCliCommand(step.command);
}

function fixtureRecipeStep(step) {
	if (step.type === 'wp-eval-file') {
		return {
			command: 'wordpress.run-php',
			args: [`code-file=${step.path}`],
		};
	}
	return {
		command: 'wordpress.wp-cli',
		args: [`command=${normalizeCliCommand(step.command)}`],
	};
}

function defaultRunCli(command, options = {}) {
	const cliPath = options.cliPath || 'wp';
	const args = [normalizeCliCommand(command)];
	if (options.sitePath) {
		args.push(`--path=${quoteShell(options.sitePath)}`);
	}

	return runShellCommand(`${cliPath} ${args.filter(Boolean).join(' ')}`, {
		cwd: options.cwd || options.sitePath || process.cwd(),
		env: options.env,
	});
}

function runShellCommand(command, options = {}) {
	return new Promise((resolve) => {
		const child = spawn(command, {
			cwd: options.cwd,
			env: { ...process.env, ...(options.env || {}) },
			shell: true,
			stdio: ['ignore', 'pipe', 'pipe'],
		});
		let stdout = '';
		let stderr = '';
		child.stdout.on('data', (chunk) => {
			stdout += chunk.toString();
		});
		child.stderr.on('data', (chunk) => {
			stderr += chunk.toString();
		});
		child.on('error', (error) => {
			resolve({ exitCode: 1, stdout, stderr: stderr || error.message, error });
		});
		child.on('close', (code, signal) => {
			resolve({ exitCode: code ?? 1, signal, stdout, stderr });
		});
	});
}

function normalizeCliResult(result) {
	if (result === undefined || result === null) {
		return { exitCode: 0, stdout: '', stderr: '' };
	}
	if (typeof result === 'string') {
		return { exitCode: 0, stdout: result, stderr: '' };
	}
	if (isPlainObject(result)) {
		let exitCode = 0;
		if (Number.isInteger(result.exitCode)) {
			exitCode = result.exitCode;
		} else if (Number.isInteger(result.code)) {
			exitCode = result.code;
		}
		return {
			...result,
			exitCode,
			stdout: result.stdout === undefined ? '' : String(result.stdout),
			stderr: result.stderr === undefined ? '' : String(result.stderr),
		};
	}
	return { exitCode: 0, stdout: String(result), stderr: '' };
}

function normalizeFixturePluginList(plugins) {
	if (plugins === undefined || plugins === null) {
		return [];
	}
	if (!Array.isArray(plugins)) {
		throw new TypeError('fixture plugins must be an array');
	}
	return plugins.map((plugin, index) => normalizeFixturePlugin(plugin, index));
}

function normalizeFixturePlugin(plugin, index) {
	const entry = typeof plugin === 'string' ? { path: plugin } : plugin;
	if (!isPlainObject(entry)) {
		throw new TypeError(`fixture plugin ${index + 1} must be a path string or object`);
	}
	if (typeof entry.path !== 'string' || entry.path.trim() === '') {
		throw new TypeError(`fixture plugin ${index + 1} requires a non-empty path`);
	}
	const pluginPath = entry.path.trim();
	const slug = typeof entry.slug === 'string' && entry.slug.trim()
		? entry.slug.trim()
		: path.basename(pluginPath);
	return {
		...entry,
		path: pluginPath,
		slug,
		plugin: entry.plugin || slug,
		activate: entry.activate !== false,
		copy: entry.copy === true,
	};
}

async function installWordPressFixturePlugins(options = {}) {
	if (!isPlainObject(options)) {
		throw new TypeError('installWordPressFixturePlugins options must be an object');
	}
	const plugins = normalizeFixturePluginList(options.plugins || options.fixturePlugins);
	if (plugins.length === 0) {
		return [];
	}
	const sitePath = typeof options.sitePath === 'string' && options.sitePath.trim()
		? options.sitePath.trim()
		: '';
	const pluginDir = options.pluginDir || (sitePath ? path.join(sitePath, 'wp-content', 'plugins') : '');
	if (!pluginDir) {
		throw new Error('installWordPressFixturePlugins requires sitePath or pluginDir');
	}

	await fsp.mkdir(pluginDir, { recursive: true });
	const installed = [];
	for (const plugin of plugins) {
		const linkPath = path.join(pluginDir, plugin.slug);
		const backupPath = `${linkPath}.homeboy-fixture-backup-${process.pid}-${Date.now()}-${installed.length}`;
		let hadExistingPath = false;

		try {
			await fsp.rename(linkPath, backupPath);
			hadExistingPath = true;
		} catch (error) {
			if (error?.code !== 'ENOENT') {
				throw error;
			}
		}

		await fsp.rm(linkPath, { recursive: true, force: true });
		if (plugin.copy) {
			await fsp.cp(plugin.path, linkPath, { recursive: true, force: true });
		} else {
			await fsp.symlink(plugin.path, linkPath, 'dir');
		}

		installed.push({
			...plugin,
			linkPath,
			backupPath,
			hadExistingPath,
		});
	}

	const route = normalizeExecutionRoute(options);
	const runCli = options.runCli || ((command, runContext) => defaultRunCli(command, {
		...options,
		...runContext,
	}));
	const runPluginStateStep = options.runPluginStateStep || (route === 'wp-codebox' && (options.runRecipeStep || options.runWpCodeboxStep)
		? async (plugin, runContext = {}) => {
			const recipeStep = wpCodeboxPluginStateStep({
				activate: [{ plugin: plugin.plugin, slug: plugin.slug }],
				report: true,
			});
			const result = await (options.runRecipeStep || options.runWpCodeboxStep)(recipeStep, {
				...runContext,
				plugin,
				recipeStep,
			});
			return { ...normalizeCliResult(result), recipeStep };
		}
		: null);
	for (const plugin of installed.filter((entry) => entry.activate)) {
		const activationCommand = `plugin activate ${plugin.plugin}`;
		const result = runPluginStateStep
			? await runPluginStateStep(plugin, {
				role: 'fixture-plugin-activate',
				timeoutMs: options.activateTimeoutMs,
			})
			: normalizeCliResult(await runCli(activationCommand, {
				plugin,
				role: 'fixture-plugin-activate',
				timeoutMs: options.activateTimeoutMs,
			}));
		if (result.exitCode !== 0) {
			throw Object.assign(failedStepError({ label: `activate:${plugin.slug}`, type: runPluginStateStep ? 'wp-codebox' : 'wp-cli' }, activationCommand, result), {
				fixturePlugin: plugin,
				installedPlugins: installed,
			});
		}
		plugin.activation = {
			command: activationCommand,
			exitCode: result.exitCode,
			stdout: result.stdout,
			stderr: result.stderr,
		};
		if (result.recipeStep) {
			plugin.activation.recipeStep = result.recipeStep;
		}
	}

	return installed.map((plugin) => ({
		slug: plugin.slug,
		plugin: plugin.plugin,
		path: plugin.path,
		copy: plugin.copy,
		activate: plugin.activate,
		linkPath: plugin.linkPath,
		backupPath: plugin.backupPath,
		hadExistingPath: plugin.hadExistingPath,
		...(plugin.activation ? { activation: plugin.activation } : {}),
	}));
}

async function restoreWordPressFixturePlugins(installedPlugins = []) {
	if (!Array.isArray(installedPlugins)) {
		throw new TypeError('installed fixture plugins must be an array');
	}
	for (const plugin of [...installedPlugins].reverse()) {
		if (!plugin?.linkPath) {
			continue;
		}
		await fsp.rm(plugin.linkPath, { recursive: true, force: true });
		if (plugin.hadExistingPath) {
			await fsp.rename(plugin.backupPath, plugin.linkPath);
		}
	}
}

async function withWordPressFixturePlugins(options, callback) {
	if (typeof callback !== 'function') {
		throw new TypeError('withWordPressFixturePlugins requires a callback');
	}
	const installedPlugins = await installWordPressFixturePlugins(options);
	try {
		return await callback(installedPlugins);
	} finally {
		await restoreWordPressFixturePlugins(installedPlugins);
	}
}

function normalizeExecutionRoute(options) {
	const route = options.fixtureExecutionRoute || options.executionRoute || options.route;
	if (route === 'wp-codebox' || route === 'codebox') {
		return 'wp-codebox';
	}
	if (route === 'host' || route === 'live-site' || route === 'live') {
		return 'host';
	}
	if (options.runRecipeStep || options.runWpCodeboxStep) {
		return 'wp-codebox';
	}
	return '';
}

function createFixtureRunner(options, context) {
	const route = normalizeExecutionRoute(options);
	if (route === 'wp-codebox') {
		const runRecipeStep = options.runRecipeStep || options.runWpCodeboxStep;
		if (typeof runRecipeStep !== 'function') {
			throw new Error('WP Codebox fixture setup requires runRecipeStep or runWpCodeboxStep.');
		}
		return async (step, stepContext) => {
			const recipeStep = fixtureRecipeStep(step);
			const result = normalizeCliResult(await runRecipeStep(recipeStep, { ...context, ...stepContext, recipeStep }));
			return {
				...result,
				recipeStep,
			};
		};
	}
	if (route === 'host') {
		const runCli = options.runCli || ((command, runContext) => defaultRunCli(command, {
			...options,
			...runContext,
		}));
		return async (step, stepContext) => runCli(fixtureCommand(step), { ...context, ...stepContext });
	}
	throw new Error('WordPress fixture setup requires an explicit execution route: fixtureExecutionRoute="wp-codebox" with runRecipeStep, or fixtureExecutionRoute="host" for live-site host wp execution.');
}

function excerpt(value) {
	const text = String(value || '').trim();
	if (!text) {
		return '';
	}
	return text.length > 4000 ? `${text.slice(0, 4000)}\n...` : text;
}

function failedStepError(step, command, result) {
	const parts = [
		`WordPress fixture step "${step.label}" failed (${step.type}: ${command}) with exit code ${result.exitCode}.`,
	];
	if (result.stdout) {
		parts.push(`STDOUT:\n${excerpt(result.stdout)}`);
	}
	if (result.stderr) {
		parts.push(`STDERR:\n${excerpt(result.stderr)}`);
	}
	return new Error(parts.join('\n'));
}

function normalizeSkipCheck(skipIf) {
	if (skipIf === undefined || skipIf === null || skipIf === false) {
		return null;
	}
	if (typeof skipIf === 'string') {
		return normalizeFixtureStep({ type: 'wp-cli', command: skipIf, label: 'idempotency check' }, 0);
	}
	if (isPlainObject(skipIf)) {
		return normalizeFixtureStep({ type: 'wp-cli', ...skipIf, label: skipIf.label || 'idempotency check' }, 0);
	}
	throw new TypeError('fixture skipIf must be a string or object');
}

async function runStep(runFixtureStep, step, context, role = 'fixture') {
	const command = fixtureCommand(step);
	const startedAt = new Date().toISOString();
	const started = Date.now();
	const result = normalizeCliResult(await runFixtureStep(step, { step, role }));
	const summary = {
		label: step.label,
		type: step.type,
		command,
		role,
		status: result.exitCode === 0 ? 'passed' : 'failed',
		exitCode: result.exitCode,
		stdout: result.stdout,
		stderr: result.stderr,
		startedAt,
		durationMs: Date.now() - started,
	};
	if (result.signal) {
		summary.signal = result.signal;
	}
	if (result.recipeStep) {
		summary.recipeStep = result.recipeStep;
	}
	if (result.exitCode !== 0) {
		throw Object.assign(failedStepError(step, command, result), { fixtureStep: summary });
	}
	return summary;
}

async function runWordPressFixtureSetup(options = {}) {
	if (!isPlainObject(options)) {
		throw new TypeError('runWordPressFixtureSetup options must be an object');
	}
	const startedAt = new Date().toISOString();
	const steps = [];
	const context = {
		sitePath: options.sitePath,
		artifactDir: options.artifactDir,
		cwd: options.cwd,
		env: options.env,
	};
	const runFixtureStep = createFixtureRunner(options, context);
	const runCli = async (command, runContext = {}) => runFixtureStep(
		normalizeFixtureStep({ type: 'wp-cli', command, label: runContext.label || 'wp-cli' }, 0),
		runContext
	);

	if (typeof options.setupWordPressFixture === 'function') {
		try {
			const calls = [];
			const capturingRunCli = async (command, callOptions = {}) => {
				const result = normalizeCliResult(await runCli(command, { ...callOptions, role: 'hook' }));
				calls.push({
					command: normalizeCliCommand(command),
					...(result.recipeStep ? { recipeStep: result.recipeStep } : {}),
					exitCode: result.exitCode,
					stdout: result.stdout,
					stderr: result.stderr,
				});
				if (result.exitCode !== 0) {
					throw failedStepError({ label: 'setupWordPressFixture', type: 'hook' }, normalizeCliCommand(command), result);
				}
				return result;
			};
			const value = await options.setupWordPressFixture({
				...context,
				runCli: capturingRunCli,
			});
			steps.push({ label: 'setupWordPressFixture', type: 'hook', status: 'passed', calls, value });
		} catch (error) {
			steps.push({ label: 'setupWordPressFixture', type: 'hook', status: 'failed', error: error.message });
			return writeFixtureSummary({ status: 'failed', steps, startedAt }, options, error);
		}
	}

	try {
		for (const step of normalizeFixtureList(options.fixtures)) {
			const skipCheck = normalizeSkipCheck(step.skipIf || step.idempotencyCheck);
			if (skipCheck) {
				const check = await runStep(runFixtureStep, skipCheck, context, 'idempotency-check').catch((error) => error.fixtureStep || {
					label: skipCheck.label,
					type: skipCheck.type,
					role: 'idempotency-check',
					status: 'failed',
					error: error.message,
				});
				if (check.status === 'passed') {
					steps.push({ ...check, fixture: step.label, status: 'skipped', reason: 'idempotency check passed' });
					continue;
				}
				steps.push({ ...check, fixture: step.label, status: 'check-failed' });
			}
			steps.push(await runStep(runFixtureStep, step, context));
		}
	} catch (error) {
		if (error.fixtureStep) {
			steps.push(error.fixtureStep);
		}
		return writeFixtureSummary({ status: 'failed', steps, startedAt }, options, error);
	}

	return writeFixtureSummary({ status: 'passed', steps, startedAt }, options);
}

function writeFixtureSummary(summary, options, error) {
	const output = {
		...summary,
		finishedAt: new Date().toISOString(),
	};
	if (options.artifactDir) {
		fs.mkdirSync(options.artifactDir, { recursive: true });
		const jsonPath = path.join(options.artifactDir, options.artifactFileName || 'wordpress-fixture-setup.json');
		fs.writeFileSync(jsonPath, `${JSON.stringify(output, null, 2)}\n`, 'utf8');
		output.artifacts = { fixtureSetup: jsonPath };
	}
	if (error) {
		error.fixtureSummary = output;
		throw error;
	}
	return output;
}

module.exports = {
	fixtureRecipeStep,
	defaultRunCli,
	installWordPressFixturePlugins,
	normalizeFixtureProfileSiteSeeds,
	normalizeFixtureList,
	normalizeFixturePluginList,
	restoreWordPressFixturePlugins,
	runWordPressFixtureSetup,
	withWordPressFixturePlugins,
};
