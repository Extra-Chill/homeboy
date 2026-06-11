export function buildWordPressBenchRecipe(options = {}) {
	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		inputs: {
			mounts: normalizeRecipeMounts(options.mounts, 'readonly'),
			extraPlugins: options.extraPlugins ?? [],
			workloads: options.workloads ?? [],
			scenarioIds: options.scenarioIds ?? [],
		},
		runtime: {
			blueprint: options.blueprint ?? {},
		},
		workflow: {
			steps: [{
				command: 'fixture.wordpress.bench',
				args: [
					`plugin-slug=${options.pluginSlug}`,
					`lifecycle-json=${JSON.stringify(options.lifecycle ?? {})}`,
					`reset-policy-json=${JSON.stringify(options.resetPolicy ?? {})}`,
				],
			}],
		},
	};
}

export function buildWordPressPhpunitRecipe(options = {}) {
	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		inputs: {
			mounts: normalizeRecipeMounts(options.mounts, 'readwrite'),
		},
		workflow: {
			steps: [{
				command: 'fixture.wordpress.phpunit',
				args: [
					`plugin-slug=${options.pluginSlug}`,
					`phpunit-args-json=${JSON.stringify(options.phpunitArgs ?? [])}`,
					`bootstrap-mode=${options.bootstrapMode ?? 'managed'}`,
					`project-bootstrap=${options.projectBootstrap ?? ''}`,
				],
			}],
		},
	};
}

function normalizeRecipeMounts(mounts = [], defaultMode) {
	return mounts.map((mount, index) => {
		if (!mount.source || typeof mount.source !== 'string') {
			throw new Error(`Recipe mount ${index} requires source`);
		}
		if (!mount.target || typeof mount.target !== 'string' || !mount.target.startsWith('/')) {
			throw new Error(`Recipe mount ${index} requires an absolute target`);
		}
		return { ...mount, mode: mount.mode ?? defaultMode };
	});
}
