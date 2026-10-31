'use strict';

const os = require('node:os');
const path = require('node:path');

function piAgentDir(env = process.env, config = {}) {
	const configured = typeof config.pi_agent_dir === 'string' ? config.pi_agent_dir.trim() : '';
	if (configured) {
		const home = env.HOME || os.homedir();
		return configured === '~' ? home : configured.startsWith('~/') ? path.join(home, configured.slice(2)) : configured;
	}
	return env.PI_CODING_AGENT_DIR || path.join(env.HOME || os.homedir(), '.pi', 'agent');
}

function piAgentEnv(env = process.env, config = {}) {
	return { ...env, PI_CODING_AGENT_DIR: piAgentDir(env, config) };
}

module.exports = { piAgentDir, piAgentEnv };
