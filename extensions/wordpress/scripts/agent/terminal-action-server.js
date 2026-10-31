#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');

const { executeTerminalAction } = require('../../lib/agent-terminal-actions');

function parseArgs(argv) {
	const args = {};
	for (let index = 0; index < argv.length; index += 1) {
		const entry = argv[index];
		if (!entry.startsWith('--')) {
			throw new Error(`Unexpected argument: ${entry}`);
		}
		const key = entry.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
		const value = argv[index + 1];
		if (!value || value.startsWith('--')) {
			throw new Error(`${entry} requires a value`);
		}
		args[key] = value;
		index += 1;
	}
	return args;
}

function readJsonRequest(request) {
	return new Promise((resolve, reject) => {
		let body = '';
		request.on('data', (chunk) => {
			body += chunk.toString();
			if (body.length > 1024 * 1024) {
				reject(new Error('Request body too large'));
				request.destroy();
			}
		});
		request.on('end', () => {
			try {
				resolve(body ? JSON.parse(body) : {});
			} catch (error) {
				reject(error);
			}
		});
		request.on('error', reject);
	});
}

function writeJson(response, statusCode, payload) {
	response.writeHead(statusCode, { 'content-type': 'application/json' });
	response.end(`${JSON.stringify(payload)}\n`);
}

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const runtimeRoot = path.resolve(args.runtimeRoot || process.cwd());
	const token = args.token || '';
	const readyFile = args.readyFile || '';
	const host = args.host || '127.0.0.1';
	const port = Number(args.port || 0);

	if (!token) {
		throw new Error('--token is required');
	}

	const server = http.createServer(async (request, response) => {
		try {
			if (request.method === 'GET' && request.url === '/health') {
				writeJson(response, 200, { ok: true });
				return;
			}

			if (request.method !== 'POST' || request.url !== '/execute') {
				writeJson(response, 404, { success: false, error: 'not_found' });
				return;
			}

			if (request.headers.authorization !== `Bearer ${token}`) {
				writeJson(response, 403, { success: false, error: 'forbidden' });
				return;
			}

			const action = await readJsonRequest(request);
			const result = await executeTerminalAction(action, {
				runtimeRoot,
				env: process.env,
			});
			writeJson(response, 200, result);
		} catch (error) {
			writeJson(response, 500, { success: false, error: error.message });
		}
	});

	server.listen(port, host, () => {
		const address = server.address();
		const url = `http://${address.address}:${address.port}`;
		if (readyFile) {
			fs.writeFileSync(readyFile, `${JSON.stringify({ url, runtimeRoot })}\n`);
		}
		console.error(`homeboy terminal action server listening on ${url}`);
	});
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
