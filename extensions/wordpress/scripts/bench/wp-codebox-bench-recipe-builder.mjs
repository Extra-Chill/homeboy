export function buildWordPressBenchRecipe(options = {}) {
	const pluginSlug = requiredPluginSlug(options.pluginSlug, 'buildWordPressBenchRecipe');
	const componentId = typeof options.componentId === 'string' && options.componentId.trim()
		? options.componentId.trim()
		: pluginSlug;

	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		runtime: {
			wp: options.wordpressVersion,
			blueprint: blueprintWithWpConfigDefines(options.blueprint ?? {}, options.wpConfigDefines ?? {}),
		},
		inputs: {
			extraPlugins: normalizeExtraPlugins(options.extraPlugins),
			mounts: normalizeRecipeMounts(options.mounts, { defaultMode: 'readonly' }),
		},
		workflow: {
			steps: [{
				command: 'wordpress.bench',
				args: [
					`component-id=${componentId}`,
					`plugin-slug=${pluginSlug}`,
					`iterations=${positiveInteger(options.iterations, 3)}`,
					`warmup=${nonNegativeInteger(options.warmupIterations, 1)}`,
					`dependency-slugs=${(options.dependencySlugs ?? []).filter(Boolean).join(',')}`,
					`env-json=${JSON.stringify(options.env ?? {})}`,
					`bootstrap-files-json=${JSON.stringify(options.bootstrapFiles ?? [])}`,
					`workloads-json=${JSON.stringify(options.workloads ?? [])}`,
				],
			}],
		},
	};
}

export function buildWordPressPhpunitRecipe(options = {}) {
	const pluginSlug = requiredPluginSlug(options.pluginSlug, 'buildWordPressPhpunitRecipe');

	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		runtime: {
			wp: options.wordpressVersion,
			blueprint: options.blueprint ?? { steps: [] },
		},
		inputs: {
			mounts: normalizeRecipeMounts(options.mounts),
		},
		workflow: {
			steps: [{
				command: 'wordpress.phpunit',
				args: [
					`plugin-slug=${pluginSlug}`,
					`test-file=${options.selectedTestFile ?? ''}`,
					`changed-tests-json=${JSON.stringify(options.changedTestFiles ?? [])}`,
					`env-json=${JSON.stringify(options.env ?? {})}`,
					`wp-config-defines-json=${JSON.stringify(options.wpConfigDefines ?? {})}`,
					`autoload-file=${options.autoloadFile ?? '/wp-codebox-vendor/autoload.php'}`,
					`tests-dir=${options.testsDir ?? '/wp-codebox-vendor/wp-phpunit/wp-phpunit'}`,
					`dependency-mounts=${(options.dependencyMounts ?? []).filter(Boolean).join(',')}`,
					`multisite=${options.multisite ? '1' : '0'}`,
				],
			}],
		},
	};
}

function normalizeRecipeMounts(mounts = [], options = {}) {
	const defaultMode = options.defaultMode ?? 'readwrite';
	return mounts.map((mount, index) => {
		if (!mount.source || typeof mount.source !== 'string') {
			throw new Error(`Recipe mount ${index} requires source`);
		}
		if (!mount.target || typeof mount.target !== 'string' || !mount.target.startsWith('/')) {
			throw new Error(`Recipe mount ${index} requires an absolute target`);
		}

		const normalized = {
			source: mount.source,
			target: mount.target,
			mode: mount.mode ?? defaultMode,
		};
		copyOptional(normalized, mount, ['type', 'metadata']);
		return normalized;
	});
}

function normalizeExtraPlugins(plugins = []) {
	return plugins.map((plugin, index) => {
		if (!plugin.source || typeof plugin.source !== 'string') {
			throw new Error(`Recipe extra plugin ${index} requires source`);
		}

		const normalized = { source: plugin.source };
		copyOptional(normalized, plugin, ['slug', 'pluginFile', 'activate', 'sha256', 'loadAs']);
		return normalized;
	});
}

function blueprintWithWpConfigDefines(blueprint, defines) {
	const defineKeys = Object.keys(defines);
	if (defineKeys.length === 0) {
		return blueprint;
	}

	if (!isPlainObject(blueprint)) {
		return { steps: [{ step: 'defineWpConfigConsts', consts: defines }] };
	}

	const existingSteps = Array.isArray(blueprint.steps) ? blueprint.steps : [];
	return {
		...blueprint,
		steps: [...existingSteps, { step: 'defineWpConfigConsts', consts: defines }],
	};
}

function requiredPluginSlug(value, caller) {
	const slug = typeof value === 'string' ? value.trim() : '';
	if (!slug) {
		throw new Error(`${caller} requires pluginSlug`);
	}
	return slug;
}

function positiveInteger(value, fallback) {
	return Number.isSafeInteger(value) && value > 0 ? value : fallback;
}

function nonNegativeInteger(value, fallback) {
	return Number.isSafeInteger(value) && value >= 0 ? value : fallback;
}

function copyOptional(target, source, keys) {
	for (const key of keys) {
		if (source[key] !== undefined) {
			target[key] = source[key];
		}
	}
}

function isPlainObject(value) {
	return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}
