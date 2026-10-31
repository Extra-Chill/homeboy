export function buildWordPressBenchRecipe(options = {}) {
	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		inputs: {
			mounts: options.mounts ?? [],
			extra_plugins: options.extra_plugins ?? [],
			pluginRuntime: options.pluginRuntime ?? {},
			workloads: options.workloads ?? [],
		},
		runtime: {
			blueprint: options.blueprint ?? {},
		},
		workflow: {
			steps: [{ command: 'wordpress.bench', args: [] }],
		},
	};
}
