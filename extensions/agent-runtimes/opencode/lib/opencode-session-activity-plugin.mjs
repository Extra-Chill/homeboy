import activity from './opencode-session-activity.js';

export default async function ({ directory }) {
	const rootPath = process.env.HOMEBOY_OPENCODE_ACTIVITY_ROOT;
	const filePath = process.env.HOMEBOY_OPENCODE_ACTIVITY_FILE;
	const taskId = process.env.HOMEBOY_OPENCODE_ACTIVITY_TASK;
	if (!rootPath || !filePath || !taskId || !directory) return {};
	const observe = activity.createOpenCodeSessionActivity({ rootPath, filePath, taskId, directory });
	return { event: async ({ event }) => observe(event) };
}
