'use strict';

// The WordPress inline archive opcache refresh (homeboy#18365) must reach PHP-FPM's
// cache over FastCGI, run the invalidation inside FPM for the swapped
// component, clean up after itself, and never fail a deploy. A fake FastCGI
// server stands in for FPM so this runs without a PHP-FPM install.
//
// The release-layout precondition (homeboy#18365) shares the same FastCGI
// client: it must pass only when every FPM socket reports opcache disabled or
// opcache.revalidate_path=On, and fail closed otherwise (Off, no socket,
// unreachable socket, no php CLI). When a php-fpm binary is installed the
// precondition is also checked against real FPM pools.

const assert = require('node:assert/strict');
const { spawnSync, spawn } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');

const manifest = JSON.parse(
	fs.readFileSync(path.resolve(__dirname, '..', 'wordpress.json'), 'utf8')
);
const policies = manifest.deploy.archive_install;
const hook = policies[0].after_switch_command;
assert.ok(hook, 'archive install declares inline opcache invalidation');
const generator = require('../scripts/deploy/archive-runtime-commands.js');
assert.equal(generator.render(fs.readFileSync(generator.MANIFEST, 'utf8')), fs.readFileSync(generator.MANIFEST, 'utf8'), 'wordpress.json commands match scripts/deploy/fpm-fastcgi.php (run archive-runtime-commands.js --write)');
const precondition = policies[0].release_layout && policies[0].release_layout.precondition_command;
assert.ok(precondition, 'plugins opt in to the release layout with a runtime precondition');
assert.ok(policies.every((policy) => JSON.stringify(policy.release_layout) === JSON.stringify(policies[0].release_layout)), 'plugins and themes share the release layout');
assert.equal(policies[0].release_layout.release_root_dir, '.homeboy-releases', 'release root is dot-prefixed so WordPress skips it when scanning');
assert.equal(policies[0].release_layout.keep_previous, 2);
assert.equal(policies[0].release_layout.keep_recent_minutes, 15);
assert.ok(precondition.includes('{{targetDir}}'), 'the precondition targets the component directory');
assert.ok(policies.every((policy) => policy.after_switch_command === hook), 'plugins and themes refresh inline');
assert.ok(!manifest.hooks['post:deploy'].some((command) => command.includes('HOMEBOY_OPCACHE_TARGET')), 'no delayed duplicate refresh');
assert.ok(hook.includes('{{targetDir}}'), 'the command targets the installed component directory');

if (spawnSync('php', ['-v']).status !== 0) {
	console.log('SKIP: php CLI not available');
	process.exit(0);
}

const work = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opcache-hook-'));
const component = path.join(work, 'plugins', 'sample-plugin');
fs.mkdirSync(path.join(component, 'inc'), { recursive: true });
fs.writeFileSync(path.join(component, 'sample-plugin.php'), '<?php\n');
fs.writeFileSync(path.join(component, 'inc', 'Provider.php'), '<?php\n');
const socket = path.join(work, 'fpm.sock');

function render(installDir, command = hook) {
	return command.split('{{targetDir}}').join(installDir);
}

function runHook(installDir, env = {}, command = hook) {
	return new Promise((resolve) => {
		const child = spawn('sh', ['-c', `export HOMEBOY_PREFIX=1 && ${render(installDir, command)}`], {
			env: { ...process.env, ...env },
		});
		let stdout = '';
		let stderr = '';
		child.stdout.on('data', (chunk) => { stdout += chunk; });
		child.stderr.on('data', (chunk) => { stderr += chunk; });
		child.on('close', (status) => resolve({ status, stdout, stderr }));
	});
}

function parseParams(buffer) {
	const params = {};
	let offset = 0;
	const length = () => {
		const first = buffer[offset];
		if (first < 128) { offset += 1; return first; }
		const value = buffer.readUInt32BE(offset) & 0x7fffffff;
		offset += 4;
		return value;
	};
	while (offset < buffer.length) {
		const nameLength = length();
		const valueLength = length();
		const name = buffer.subarray(offset, offset + nameLength).toString();
		offset += nameLength;
		params[name] = buffer.subarray(offset, offset + valueLength).toString();
		offset += valueLength;
	}
	return params;
}

function record(type, content) {
	const pad = (8 - (content.length % 8)) % 8;
	const header = Buffer.from([1, type, 0, 1, content.length >> 8, content.length & 0xff, pad, 0]);
	return Buffer.concat([header, content, Buffer.alloc(pad)]);
}

function fakeFpm(socketPath = socket, respond = () => 'invalidated 2 file(s)') {
	const seen = [];
	const server = net.createServer((connection) => {
		let buffered = Buffer.alloc(0);
		let params = Buffer.alloc(0);
		connection.on('data', (chunk) => {
			buffered = Buffer.concat([buffered, chunk]);
			while (buffered.length >= 8) {
				const type = buffered[1];
				const contentLength = buffered.readUInt16BE(4);
				const total = 8 + contentLength + buffered[6];
				if (buffered.length < total) { return; }
				const content = buffered.subarray(8, 8 + contentLength);
				buffered = buffered.subarray(total);
				if (type === 4) { params = Buffer.concat([params, content]); }
				if (type === 5 && contentLength === 0) {
					const request = parseParams(params);
					const worker = request.SCRIPT_FILENAME;
					seen.push({ request, workerExisted: fs.existsSync(worker), workerSource: fs.existsSync(worker) ? fs.readFileSync(worker, 'utf8') : '' });
					connection.write(record(6, Buffer.from(`Content-type: text/html\r\n\r\n${respond(request)}`)));
					connection.end(record(3, Buffer.from([0, 0, 0, 0, 0, 0, 0, 0])));
				}
			}
		});
	});
	return new Promise((resolve) => server.listen(socketPath, () => resolve({ server, seen })));
}

const runPrecondition = (installDir, env) => runHook(installDir, env, precondition);

async function checkPreconditionWithFakeFpm() {
	const sockets = {};
	for (const answer of ['revalidate_path=on', 'revalidate_path=off', 'opcache=disabled', 'something else']) {
		const socketPath = path.join(work, `${answer.replace(/[^a-z]+/g, '-')}.sock`);
		sockets[answer] = { path: socketPath, ...(await fakeFpm(socketPath, () => answer)) };
	}
	const firstInstall = path.join(work, 'plugins', 'not-yet-installed');
	try {
		const on = await runPrecondition(component, { HOMEBOY_PHP_FPM_SOCKET: sockets['revalidate_path=on'].path });
		assert.equal(on.status, 0, `revalidate_path=On passes: ${on.stdout}${on.stderr}`);
		assert.match(on.stdout, /release precondition: passed for 1 PHP-FPM socket/);
		const { request, workerExisted, workerSource } = sockets['revalidate_path=on'].seen[0];
		assert.ok(workerExisted, 'check worker exists while FPM runs it');
		assert.match(path.basename(request.SCRIPT_FILENAME), /^\.homeboy-fpm-check-[0-9a-f]{16}\.php$/);
		assert.equal(path.dirname(request.SCRIPT_FILENAME), fs.realpathSync(path.dirname(component)), 'check worker sits beside the target');
		assert.ok(workerSource.includes("ini_get('opcache.revalidate_path')"), 'FPM itself reports revalidate_path');
		assert.equal(fs.existsSync(request.SCRIPT_FILENAME), false, 'check worker is removed afterwards');

		const firstDeploy = await runPrecondition(firstInstall, { HOMEBOY_PHP_FPM_SOCKET: sockets['revalidate_path=on'].path });
		assert.equal(firstDeploy.status, 0, `a first install (no target yet) can still be checked: ${firstDeploy.stdout}`);

		const disabled = await runPrecondition(component, { HOMEBOY_PHP_FPM_SOCKET: sockets['opcache=disabled'].path });
		assert.equal(disabled.status, 0, 'opcache disabled is safe for a symlink flip');

		const off = await runPrecondition(component, { HOMEBOY_PHP_FPM_SOCKET: sockets['revalidate_path=off'].path });
		assert.equal(off.status, 1, 'revalidate_path=Off fails the precondition');
		assert.match(off.stdout, /revalidate_path=off/);
		assert.match(off.stdout, /set opcache\.revalidate_path=1 in every FPM pool/);

		const mixed = await runPrecondition(component, {
			HOMEBOY_PHP_FPM_SOCKET: `${sockets['revalidate_path=on'].path},${sockets['revalidate_path=off'].path}`,
		});
		assert.equal(mixed.status, 1, 'every socket must pass');
		assert.match(mixed.stdout, /1 of 2 PHP-FPM socket/);

		const unknown = await runPrecondition(component, { HOMEBOY_PHP_FPM_SOCKET: sockets['something else'].path });
		assert.equal(unknown.status, 1, 'an unrecognised answer fails closed');

		const dead = await runPrecondition(component, { HOMEBOY_PHP_FPM_SOCKET: path.join(work, 'missing.sock') });
		assert.equal(dead.status, 1, 'an unreachable socket fails closed');
		assert.match(dead.stdout, /connect failed/);

		// No socket at all. A configured list that names no socket exercises the
		// same branch deterministically; host discovery is exercised too when
		// this host has no FPM socket under the default directories.
		const none = await runPrecondition(component, { HOMEBOY_PHP_FPM_SOCKET: ' , ' });
		assert.equal(none.status, 1, 'no FPM socket fails closed');
		assert.match(none.stdout, /no writable PHP-FPM socket found.*failing closed/);
		const discoverable = ['/run/php', '/var/run/php', '/run/php-fpm', '/var/run/php-fpm']
			.some((dir) => fs.existsSync(dir) && fs.readdirSync(dir).some((name) => name.endsWith('.sock')));
		if (!discoverable) {
			const undiscovered = await runPrecondition(component, {});
			assert.equal(undiscovered.status, 1, 'no discoverable FPM socket fails closed');
			assert.match(undiscovered.stdout, /no writable PHP-FPM socket found.*failing closed/);
		}

		const noPhp = spawnSync('/bin/sh', ['-c', render(component, precondition)], { env: { PATH: path.join(work, 'empty-bin') }, encoding: 'utf8' });
		assert.equal(noPhp.status, 1, 'a host without a php CLI fails closed');
		assert.match(noPhp.stdout, /php CLI not found; failing closed/);
		assert.deepEqual(
			fs.readdirSync(path.dirname(component)).filter((name) => name.startsWith('.homeboy-fpm-check-')),
			[],
			'no check workers are left behind'
		);
	} finally {
		for (const { server } of Object.values(sockets)) { server.close(); }
	}
}

// Real PHP-FPM pools, when a php-fpm binary is available: the precondition
// must read the pool's effective opcache.revalidate_path.
function findPhpFpm() {
	const candidates = [process.env.HOMEBOY_TEST_PHP_FPM, 'php-fpm'];
	for (const dir of ['/usr/sbin', '/usr/local/sbin']) {
		if (fs.existsSync(dir)) {
			candidates.push(...fs.readdirSync(dir).filter((name) => /^php-fpm[0-9.]*$/.test(name)).map((name) => path.join(dir, name)));
		}
	}
	return candidates.find((candidate) => candidate && spawnSync(candidate, ['-v']).status === 0);
}

async function checkPreconditionWithRealFpm() {
	const fpm = findPhpFpm();
	if (!fpm) {
		console.log('SKIP: php-fpm not available; real-pool precondition check skipped');
		return;
	}
	const waitFor = async (file) => {
		for (let i = 0; i < 100 && !fs.existsSync(file); i += 1) {
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
		return fs.existsSync(file);
	};
	for (const [revalidate, expected] of [['1', 0], ['0', 1]]) {
		const dir = fs.mkdtempSync(path.join(work, `fpm-${revalidate}-`));
		const poolSocket = path.join(dir, 'pool.sock');
		const config = path.join(dir, 'php-fpm.conf');
		fs.writeFileSync(config, [
			'[global]',
			`pid = ${path.join(dir, 'fpm.pid')}`,
			`error_log = ${path.join(dir, 'fpm.log')}`,
			'daemonize = no',
			'[pool]',
			`listen = ${poolSocket}`,
			'pm = static',
			'pm.max_children = 1',
			'php_admin_value[opcache.enable] = 1',
			`php_admin_value[opcache.revalidate_path] = ${revalidate}`,
			'',
		].join('\n'));
		const child = spawn(fpm, ['-n', '-R', '-y', config, '-F', '-d', 'zend_extension=opcache'], { stdio: 'ignore' });
		try {
			if (!(await waitFor(poolSocket))) {
				console.log(`SKIP: ${fpm} could not start a test pool; real-pool precondition check skipped`);
				return;
			}
			const result = await runPrecondition(component, { HOMEBOY_PHP_FPM_SOCKET: poolSocket });
			if (/opcache=disabled/.test(result.stdout)) {
				console.log('SKIP: opcache not loadable in the test php-fpm pool');
				return;
			}
			assert.equal(result.status, expected, `real FPM with revalidate_path=${revalidate}: ${result.stdout}${result.stderr}`);
			assert.match(result.stdout, revalidate === '1' ? /revalidate_path=on/ : /revalidate_path=off/);
		} finally {
			child.kill('SIGTERM');
			await new Promise((resolve) => child.once('exit', resolve));
		}
	}
	console.log('real php-fpm precondition check passed');
}

(async () => {
	const { server, seen } = await fakeFpm();
	try {
		const result = await runHook(component, { HOMEBOY_PHP_FPM_SOCKET: socket });
		assert.equal(result.status, 0, `hook exits 0: ${result.stderr}`);
		assert.match(result.stdout, new RegExp(`opcache: ${socket.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}: invalidated 2 file\\(s\\)`));
		assert.equal(seen.length, 1, 'one FastCGI request per socket');

		const { request, workerExisted, workerSource } = seen[0];
		const realComponent = fs.realpathSync(component);
		assert.equal(request.REQUEST_METHOD, 'GET');
		assert.equal(path.dirname(request.SCRIPT_FILENAME), path.dirname(realComponent), 'worker sits beside the component, inside the FPM-readable tree');
		assert.match(path.basename(request.SCRIPT_FILENAME), /^\.homeboy-opcache-[0-9a-f]{16}\.php$/, 'worker is a randomly named dotfile');
		assert.ok(workerExisted, 'worker exists while FPM runs it');
		assert.ok(workerSource.includes(`$root = '${realComponent}'`), 'worker targets the resolved component path');
		assert.ok(workerSource.includes('opcache_invalidate($file, true)'), 'worker force-invalidates inside FPM');
		assert.ok(workerSource.includes('opcache_get_status(true)'), 'worker also drops cached scripts that no longer exist on disk');
		assert.equal(fs.existsSync(request.SCRIPT_FILENAME), false, 'worker is removed afterwards');
		assert.deepEqual(
			fs.readdirSync(path.dirname(realComponent)).filter((name) => name.startsWith('.homeboy-opcache-')),
			[],
			'no worker files are left behind'
		);

		const missingTarget = await runHook(path.join(work, 'missing'), { HOMEBOY_PHP_FPM_SOCKET: socket });
		assert.equal(missingTarget.status, 0);
		assert.match(missingTarget.stdout, /not found; skipped/);

		const deadSocket = await runHook(component, { HOMEBOY_PHP_FPM_SOCKET: path.join(work, 'missing.sock') });
		assert.equal(deadSocket.status, 0, 'an unreachable FPM never fails the deploy');
		assert.match(deadSocket.stdout, /connect failed/);

		const noPhp = spawnSync('/bin/sh', ['-c', render(component)], { env: { PATH: path.join(work, 'empty-bin') }, encoding: 'utf8' });
		assert.equal(noPhp.status, 0, 'a host without a php CLI never fails the deploy');
		assert.match(noPhp.stdout, /php CLI not found; skipped/);

		await checkPreconditionWithFakeFpm();
		await checkPreconditionWithRealFpm();
	} finally {
		server.close();
		fs.rmSync(work, { recursive: true, force: true });
	}
	console.log('opcache invalidation hook smoke passed');
})().catch((error) => {
	console.error(error);
	process.exit(1);
});
