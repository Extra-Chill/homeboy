'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

const fixtureDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-terminal-server-'));
const readyFile = path.join(fixtureDir, 'ready.json');
const binDir = path.join(fixtureDir, 'bin');
const token = 'smoke-token';

fs.mkdirSync(binDir, { recursive: true });
fs.writeFileSync(path.join(binDir, 'wp'), '#!/usr/bin/env bash\nprintf "wp:%s\\n" "$*"\n', { mode: 0o755 });

function waitForReady() {
	return new Promise((resolve, reject) => {
		const deadline = Date.now() + 10000;
		const poll = () => {
			if (fs.existsSync(readyFile)) {
				resolve(JSON.parse(fs.readFileSync(readyFile, 'utf8')));
				return;
			}
			if (Date.now() > deadline) {
				reject(new Error('terminal action server did not become ready'));
				return;
			}
			setTimeout(poll, 50);
		};
		poll();
	});
}

function postJson(url, payload, bearer = token) {
	return new Promise((resolve, reject) => {
		const body = JSON.stringify(payload);
		const request = http.request(`${url}/execute`, {
			method: 'POST',
			headers: {
				authorization: `Bearer ${bearer}`,
				'content-type': 'application/json',
				'content-length': Buffer.byteLength(body),
			},
		}, (response) => {
			let text = '';
			response.on('data', (chunk) => {
				text += chunk.toString();
			});
			response.on('end', () => {
				resolve({ statusCode: response.statusCode, body: JSON.parse(text) });
			});
		});
		request.on('error', reject);
		request.end(body);
	});
}

async function main() {
	const child = spawn(process.execPath, [
		path.join(__dirname, '../scripts/agent/terminal-action-server.js'),
		'--runtime-root', fixtureDir,
		'--ready-file', readyFile,
		'--token', token,
	], {
		env: { ...process.env, PATH: `${binDir}:${process.env.PATH || ''}` },
		stdio: ['ignore', 'ignore', 'pipe'],
	});

	try {
		const ready = await waitForReady();
		const response = await postJson(ready.url, { type: 'wp_cli', command: 'option get blogname' });
		assert.equal(response.statusCode, 200);
		assert.equal(response.body.success, true);
		assert.equal(response.body.command, 'wp option get blogname');
		assert.match(response.body.stdout, /wp:option get blogname/);

		const forbidden = await postJson(ready.url, { type: 'wp_cli', command: 'option get blogname' }, 'wrong-token');
		assert.equal(forbidden.statusCode, 403);

		console.log('Terminal action server smoke passed.');
	} finally {
		child.kill('SIGTERM');
		fs.rmSync(fixtureDir, { recursive: true, force: true });
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
