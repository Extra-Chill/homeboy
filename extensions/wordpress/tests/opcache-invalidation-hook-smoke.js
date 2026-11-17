'use strict';

// The WordPress inline archive opcache refresh (homeboy#18365) must reach PHP-FPM's
// cache over FastCGI, run the invalidation inside FPM for the swapped
// component, clean up after itself, and never fail a deploy. A fake FastCGI
// server stands in for FPM so this runs without a PHP-FPM install.

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

function render(installDir) {
	return hook.split('{{targetDir}}').join(installDir);
}

function runHook(installDir, env = {}) {
	return new Promise((resolve) => {
		const child = spawn('sh', ['-c', `export HOMEBOY_PREFIX=1 && ${render(installDir)}`], {
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

function fakeFpm() {
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
					connection.write(record(6, Buffer.from('Content-type: text/html\r\n\r\ninvalidated 2 file(s)')));
					connection.end(record(3, Buffer.from([0, 0, 0, 0, 0, 0, 0, 0])));
				}
			}
		});
	});
	return new Promise((resolve) => server.listen(socket, () => resolve({ server, seen })));
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
	} finally {
		server.close();
		fs.rmSync(work, { recursive: true, force: true });
	}
	console.log('opcache invalidation hook smoke passed');
})().catch((error) => {
	console.error(error);
	process.exit(1);
});
