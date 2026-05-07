'use strict';

const assert = require('node:assert/strict');
const http = require('node:http');
const { EventEmitter } = require('node:events');

const {
	DEFAULT_READINESS_PATH,
	DEFAULT_DIAGNOSTIC_PATHS,
	waitForWordPressReady,
	probeWordPressDiagnostics,
} = require('../lib/playground-readiness');

function startServer(handler) {
	return new Promise((resolve) => {
		const server = http.createServer(handler);
		server.listen(0, '127.0.0.1', () => {
			const address = server.address();
			resolve({ server, port: address.port, baseUrl: `http://127.0.0.1:${address.port}` });
		});
	});
}

function closeServer(server) {
	return new Promise((resolve) => {
		if (!server) return resolve();
		server.close(() => resolve());
		// Force any keep-alive sockets closed quickly.
		if (typeof server.closeAllConnections === 'function') {
			server.closeAllConnections();
		}
	});
}

async function scenarioSelfRedirectButWpJsonReady() {
	const { server, baseUrl } = await startServer((req, res) => {
		if (req.url === '/wp-json/') {
			res.writeHead(200, { 'Content-Type': 'application/json' });
			res.end('{"namespaces":[]}');
			return;
		}
		// Self-redirect at root: would hang Node fetch. Helper must NOT follow.
		res.writeHead(302, { Location: '/', 'Content-Type': 'text/plain' });
		res.end('redirect');
	});
	try {
		const result = await waitForWordPressReady(baseUrl, {
			intervalMs: 50,
			requestTimeoutMs: 1000,
			timeoutMs: 5000,
		});
		assert.equal(result.status, 'ready', 'expected ready status');
		assert.equal(result.http_status, 200);
		assert.ok(result.url.endsWith(DEFAULT_READINESS_PATH), `url should end with ${DEFAULT_READINESS_PATH}, got ${result.url}`);
		assert.ok(Array.isArray(result.status_history));
		assert.ok(Array.isArray(result.redirect_history));
		assert.ok(result.elapsedMs < 5000, 'should resolve well under timeout');
	} finally {
		await closeServer(server);
	}
}

async function scenarioAlways503() {
	const { server, baseUrl } = await startServer((req, res) => {
		res.writeHead(503, { 'Content-Type': 'text/plain' });
		res.end('service unavailable');
	});
	try {
		let threw = false;
		try {
			await waitForWordPressReady(baseUrl, {
				intervalMs: 50,
				requestTimeoutMs: 500,
				timeoutMs: 1500,
				playgroundOutput: () => 'some captured stdout/stderr from playground process\n',
			});
		} catch (err) {
			threw = true;
			assert.ok(err instanceof Error);
			assert.match(err.message, /Timed out waiting for/);
			const diag = err.diagnostics;
			assert.ok(diag, 'diagnostics must be attached');
			assert.equal(diag.timeoutMs, 1500);
			assert.ok(diag.attempts >= 1, 'should record at least one attempt');
			assert.ok(diag.routes && Array.isArray(diag.routes.paths));
			assert.equal(diag.routes.paths.length, DEFAULT_DIAGNOSTIC_PATHS.length);
			assert.equal(diag.routes.paths.length, 4);
			assert.ok(diag.tcp && diag.tcp.open === true, 'tcp probe should report open');
			assert.equal(typeof diag.playgroundOutputTail, 'string');
			assert.ok(diag.playgroundOutputTail.length > 0);
			assert.equal(diag.playground, null);
		}
		assert.ok(threw, 'always-503 must throw');
	} finally {
		await closeServer(server);
	}
}

async function scenarioSelfRedirectReadyOptIn() {
	const { server, baseUrl } = await startServer((req, res) => {
		res.writeHead(302, { Location: req.url, 'Content-Type': 'text/plain' });
		res.end('playground login redirect');
	});
	try {
		const strictTimeout = await waitForWordPressReady(baseUrl, {
			intervalMs: 50,
			requestTimeoutMs: 500,
			timeoutMs: 250,
		}).then(
			() => false,
			(err) => /Timed out waiting for/.test(err.message)
		);
		assert.equal(strictTimeout, true, 'same-path redirects must not be ready unless the caller opts in');

		const result = await waitForWordPressReady(baseUrl, {
			intervalMs: 50,
			requestTimeoutMs: 500,
			timeoutMs: 1000,
			readyOnSelfRedirect: true,
		});
		assert.equal(result.status, 'ready');
		assert.equal(result.http_status, 302);
		assert.equal(result.ready_reason, 'self_redirect');
		assert.equal(result.redirect_history.length, 1);
		assert.equal(result.redirect_history[0].self, true);
	} finally {
		await closeServer(server);
	}
}

async function scenarioDifferentRedirectStillNotReady() {
	const { server, baseUrl } = await startServer((req, res) => {
		res.writeHead(302, { Location: '/wp-login.php', 'Content-Type': 'text/plain' });
		res.end('not same path');
	});
	try {
		const timedOut = await waitForWordPressReady(baseUrl, {
			intervalMs: 50,
			requestTimeoutMs: 500,
			timeoutMs: 250,
			readyOnSelfRedirect: true,
		}).then(
			() => false,
			(err) => /Timed out waiting for/.test(err.message)
		);
		assert.equal(timedOut, true, 'redirects to a different path are not readiness');
	} finally {
		await closeServer(server);
	}
}

async function scenarioProcessExitsMidPoll() {
	// Server returns 503 forever so readiness never succeeds.
	const { server, baseUrl } = await startServer((req, res) => {
		res.writeHead(503, { 'Content-Type': 'text/plain' });
		res.end('not ready');
	});

	const fakeChild = new EventEmitter();
	fakeChild.pid = 99999;
	fakeChild.exitCode = null;
	fakeChild.signalCode = null;

	const exitTimer = setTimeout(() => {
		fakeChild.exitCode = 1;
		fakeChild.signalCode = null;
		fakeChild.emit('exit', 1, null);
	}, 300);

	try {
		const result = await waitForWordPressReady(baseUrl, {
			intervalMs: 100,
			requestTimeoutMs: 500,
			timeoutMs: 10000,
			playgroundProcess: fakeChild,
		});
		assert.equal(result.status, 'process_exited');
		assert.ok(result.playground);
		assert.equal(result.playground.exitCode, 1);
		assert.equal(result.playground.pid, 99999);
		assert.equal(result.http_status, null);
	} finally {
		clearTimeout(exitTimer);
		await closeServer(server);
	}
}

async function scenarioDiagnosticsStandalone() {
	const hits = Object.create(null);
	const { server, baseUrl } = await startServer((req, res) => {
		hits[req.url] = (hits[req.url] || 0) + 1;
		if (req.url === '/') {
			res.writeHead(302, { Location: '/', 'Content-Type': 'text/plain' });
			res.end('redirect-loop');
			return;
		}
		if (req.url === '/wp-login.php') {
			res.writeHead(200, { 'Content-Type': 'text/html' });
			res.end('<html><body>login form</body></html>');
			return;
		}
		if (req.url === '/wp-json/') {
			res.writeHead(200, { 'Content-Type': 'application/json' });
			res.end('{"namespaces":[]}');
			return;
		}
		if (req.url === '/wp-admin/') {
			res.writeHead(302, { Location: '/wp-login.php', 'Content-Type': 'text/plain' });
			res.end('redirect');
			return;
		}
		res.writeHead(404);
		res.end();
	});

	try {
		const result = await probeWordPressDiagnostics(baseUrl, { requestTimeoutMs: 1000 });
		assert.equal(result.paths.length, 4);

		const byPath = Object.fromEntries(result.paths.map((entry) => [entry.path, entry]));

		assert.equal(byPath['/'].status, 302);
		assert.equal(byPath['/'].redirectLocation, '/');
		assert.ok(byPath['/'].contentType && byPath['/'].contentType.includes('text/plain'));

		assert.equal(byPath['/wp-login.php'].status, 200);
		assert.equal(byPath['/wp-login.php'].redirectLocation, null);
		assert.ok(byPath['/wp-login.php'].contentType && byPath['/wp-login.php'].contentType.includes('text/html'));

		assert.equal(byPath['/wp-json/'].status, 200);
		assert.equal(byPath['/wp-json/'].redirectLocation, null);

		assert.equal(byPath['/wp-admin/'].status, 302);
		assert.equal(byPath['/wp-admin/'].redirectLocation, '/wp-login.php');

		for (const entry of result.paths) {
			assert.ok(typeof entry.bodyPreview === 'string', 'bodyPreview must be a string');
			assert.ok(entry.bodyPreview.length <= 500, 'bodyPreview must be <=500 chars');
			assert.equal(entry.error, undefined, `unexpected error on ${entry.path}`);
		}

		// Each diagnostic path must have been hit exactly once: no redirect-following.
		for (const probePath of DEFAULT_DIAGNOSTIC_PATHS) {
			assert.equal(hits[probePath], 1, `path ${probePath} should be hit exactly once, got ${hits[probePath]}`);
		}
	} finally {
		await closeServer(server);
	}
}

(async () => {
	await scenarioSelfRedirectButWpJsonReady();
	await scenarioAlways503();
	await scenarioSelfRedirectReadyOptIn();
	await scenarioDifferentRedirectStillNotReady();
	await scenarioProcessExitsMidPoll();
	await scenarioDiagnosticsStandalone();
	console.log('Playground readiness smoke passed.');
})().catch((err) => {
	console.error(err && err.stack ? err.stack : err);
	process.exitCode = 1;
});
