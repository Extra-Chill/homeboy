export function buildWordPressBenchRecipe(options = {}) {
	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		inputs: {
			mounts: normalizeRecipeMounts(options.mounts, 'readonly'),
			extra_plugins: options.extra_plugins ?? [],
			workloads: options.workloads ?? [],
			scenarioIds: options.scenarioIds ?? [],
		},
		runtime: {
			blueprint: options.blueprint ?? {},
		},
		workflow: {
			...(Array.isArray(options.prepareSteps) && options.prepareSteps.length > 0 ? { before: options.prepareSteps } : {}),
			steps: [{
				command: 'fixture.wordpress.bench',
				args: [
					`plugin-slug=${options.pluginSlug}`,
					`lifecycle-json=${JSON.stringify(options.lifecycle ?? {})}`,
					`reset-policy-json=${JSON.stringify(options.resetPolicy ?? {})}`,
				],
			}, ...(options.postSteps ?? [])],
		},
	};
}

export function buildWordPressPhpunitRecipe(options = {}) {
	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		inputs: {
			extra_plugins: options.extra_plugins ?? [],
			mounts: normalizeRecipeMounts(options.mounts, 'readonly'),
		},
		workflow: {
			steps: [{
				command: 'fixture.wordpress.phpunit',
				args: [
					`plugin-slug=${options.pluginSlug}`,
					`cwd=${options.cwd ?? ''}`,
					`test-root=${options.testRoot ?? ''}`,
					`phpunit-xml=${options.phpunitXml ?? ''}`,
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
