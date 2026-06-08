export function buildWordPressBenchRecipe(options = {}) {
	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		inputs: {
			mounts: options.mounts ?? [],
			extraPlugins: options.extraPlugins ?? [],
			pluginRuntime: options.pluginRuntime ?? {},
		},
		runtime: {
			blueprint: options.blueprint ?? {},
		},
		workflow: {
			steps: [{ command: 'wordpress.bench', args: [] }],
		},
	};
}
