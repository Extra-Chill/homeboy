'use strict';

// Real Homeboy admission and executor boundary, with disposable native accounts
// and a deterministic CLI. No network requests or operator credentials.
require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const homeboy = process.env.HOMEBOY_BIN || 'homeboy';
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-auth-'));
const runtime = path.resolve(__dirname, '..');
const home = path.join(root, 'home');
const bin = path.join(root, 'bin');
const workspace = path.join(root, 'workspace');
const dataHome = path.join(home, 'native-data');
const store = path.join(dataHome, 'opencode/auth.json');
const marker = path.join(root, 'executed.jsonl');
const env = {
	PATH: `${bin}${path.delimiter}${process.env.PATH}`,
	HOME: home,
	XDG_CONFIG_HOME: path.join(home, '.config'),
	XDG_DATA_HOME: dataHome,
	XDG_STATE_HOME: path.join(home, '.local/state'),
	HOMEBOY_DATA_DIR: path.join(home, 'data'),
	HOMEBOY_RUNTIME_CONTRACT_CONSTANTS_FIXTURE: process.env.HOMEBOY_RUNTIME_CONTRACT_CONSTANTS_FIXTURE,
};

function homeboyCommand(args, input) {
	const result = spawnSync(homeboy, ['--placement', 'local', ...args], {
		env, input: input && JSON.stringify(input), encoding: 'utf8', timeout: 60_000,
	});
	assert.equal(result.error, undefined);
	return result;
}

function plan(authKind, secretEnv = []) {
	return {
		schema: 'homeboy/agent-task-plan/v1', plan_id: `auth-${authKind || 'native'}`,
		tasks: [{
			schema: 'homeboy/agent-task-request/v1', task_id: `auth-${authKind || 'native'}`,
			executor: {
				backend: 'opencode', model: 'openai/gpt-6.1-sol',
				secret_env: secretEnv,
				config: { ...(authKind ? { provider: 'openai', auth_kind: authKind } : {}) },
			},
			workspace: { root: workspace }, instructions: 'Reply with READY.',
			policy: { write: 'none' },
		}],
	};
}

try {
	for (const directory of [bin, workspace, path.dirname(store), path.join(home, '.config/homeboy/agent-runtimes')]) {
		fs.mkdirSync(directory, { recursive: true });
	}
	fs.symlinkSync(runtime, path.join(home, '.config/homeboy/agent-runtimes/opencode'), 'dir');
	fs.writeFileSync(path.join(bin, 'opencode'), `#!${process.execPath}
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
const config = JSON.parse(process.env.OPENCODE_CONFIG_CONTENT || '{}');
const nativeStore = path.join(process.env.XDG_DATA_HOME || path.join(process.env.HOME, '.local/share'), 'opencode/auth.json');
assert.equal(nativeStore, ${JSON.stringify(store)}, 'readiness and execution must select the same native store');
const stored = JSON.parse(fs.readFileSync(nativeStore, 'utf8')).openai;
if (args[0] === '--version') console.log('1.18.29');
else if (args[0] === 'auth') console.log(stored ? 'openai ' + stored.type : process.env.OPENAI_API_KEY ? 'openai api' : '');
else if (args[0] === 'models') console.log('openai/gpt-6.1-sol');
else if (args[0] === 'debug') console.log(JSON.stringify({ permission: config.agent.build.permission }));
else if (args[0] === 'run') {
  assert.equal(args[args.indexOf('--model') + 1], 'openai/gpt-6.1-sol');
  const auth = stored;
  if (auth) assert.equal(process.env.OPENAI_API_KEY, undefined);
  else assert.equal(process.env.OPENAI_API_KEY, 'fixture-api-key');
  if (!args.includes('homeboy-readiness')) fs.appendFileSync(${JSON.stringify(marker)}, JSON.stringify({model: config.model, auth: auth?.type || 'api_key'}) + '\\n');
  console.log(JSON.stringify({type:'text',part:{text:'READY'}}));
} else process.exit(1);
`, { mode: 0o700 });
	const gitEnv = { ...env, GIT_AUTHOR_NAME: 'Test', GIT_AUTHOR_EMAIL: 'test@example.test', GIT_COMMITTER_NAME: 'Test', GIT_COMMITTER_EMAIL: 'test@example.test' };
	for (const args of [['init'], ['commit', '--allow-empty', '-m', 'fixture']]) {
		assert.equal(spawnSync('git', args, { cwd: workspace, env: gitEnv, encoding: 'utf8' }).status, 0);
	}
	// Expired access prevents the capacity reader calling a live usage endpoint.
	fs.writeFileSync(store, JSON.stringify({ openai: { type: 'oauth', access: 'fixture-access', refresh: 'fixture-refresh', expires: 1 } }));
	const validation = homeboyCommand(['agent-task', 'validate-plan', '--plan', '-'], plan());
	assert.equal(validation.status, 0, validation.stderr + validation.stdout);
	const execution = homeboyCommand(['agent-task', 'run-plan', '--wait', '--plan', '-'], plan());
	assert.equal(execution.status, 0, execution.stderr + execution.stdout);
	assert.equal(fs.existsSync(marker), true, 'OAuth route must reach the actual executor CLI');
	assert.equal(JSON.parse(fs.readFileSync(marker, 'utf8').trim()).auth, 'oauth');
	fs.unlinkSync(marker);
	fs.writeFileSync(store, JSON.stringify({ openai: { type: 'api', key: 'fixture-native-key' } }));
	const nativeApi = homeboyCommand(['agent-task', 'run-plan', '--wait', '--plan', '-'], plan());
	assert.equal(nativeApi.status, 0, nativeApi.stderr + nativeApi.stdout);
	assert.equal(JSON.parse(fs.readFileSync(marker, 'utf8').trim()).auth, 'api');
	assert.equal((nativeApi.stdout + nativeApi.stderr).includes('fixture-native-key'), false);
	fs.unlinkSync(marker);
	const selectedNativeApi = homeboyCommand(['agent-task', 'run-plan', '--wait', '--plan', '-'], plan('api_key'));
	assert.equal(selectedNativeApi.status, 0, selectedNativeApi.stderr + selectedNativeApi.stdout);
	assert.equal(JSON.parse(fs.readFileSync(marker, 'utf8').trim()).auth, 'api');
	fs.unlinkSync(marker);

	fs.writeFileSync(store, '{}');
	env.OPENAI_API_KEY = 'fixture-api-key';
	const apiExecution = homeboyCommand(['agent-task', 'run-plan', '--wait', '--plan', '-'], plan('api_key', ['OPENAI_API_KEY']));
	assert.equal(apiExecution.status, 0, apiExecution.stderr + apiExecution.stdout);
	assert.equal(JSON.parse(fs.readFileSync(marker, 'utf8').trim()).auth, 'api_key');
	assert.equal((apiExecution.stdout + apiExecution.stderr).includes('fixture-api-key'), false);
	fs.unlinkSync(marker);
	delete env.OPENAI_API_KEY;
	const missingKey = homeboyCommand(['agent-task', 'validate-plan', '--plan', '-'], plan('api_key'));
	assert.notEqual(missingKey.status, 0, 'explicit API-key account must not admit without its key');
	assert.equal(fs.existsSync(marker), false);
	const missingDeclaredKey = homeboyCommand(['agent-task', 'validate-plan', '--plan', '-'], plan('api_key', ['OPENAI_API_KEY']));
	assert.notEqual(missingDeclaredKey.status, 0, 'a declared scoped secret remains mandatory');
	const missingNative = homeboyCommand(['agent-task', 'validate-plan', '--plan', '-'], plan());
	assert.notEqual(missingNative.status, 0, 'native route must not admit without native credentials');
	console.log('Homeboy OAuth-only admission/execution and selected API-key rejection passed');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
