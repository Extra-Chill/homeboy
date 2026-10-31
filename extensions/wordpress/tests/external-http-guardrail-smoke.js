'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	DEFAULT_EXTERNAL_HTTP_GUARDRAIL_ARTIFACT_RELATIVE_PATH,
	collectWordPressExternalHttpGuardrailEvents,
	generateExternalHttpGuardrailPlugin,
	installWordPressExternalHttpGuardrail,
	normalizeExternalHttpGuardrailPolicy,
	parseWordPressExternalHttpGuardrailJsonl,
	redactExternalHttpGuardrailUrl,
	resolveExternalHttpGuardrailPaths,
	summarizeWordPressExternalHttpGuardrailEvents,
	uninstallWordPressExternalHttpGuardrail,
} = require('../lib/external-http-guardrail');

const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-external-http-'));

try {
	fs.mkdirSync(path.join(fixture, 'wp-content'), { recursive: true });

	const plugin = generateExternalHttpGuardrailPlugin({
		allowlistDomains: ['api.wordpress.org', 'example.test'],
		blockResponse: { code: 598, message: 'Blocked in smoke', body: '{"blocked":true}' },
	});
	assert.match(plugin, /Plugin Name: Homeboy External HTTP Guardrail/);
	assert.match(plugin, /pre_http_request/);
	assert.match(plugin, /http\.blocked/);
	assert.match(plugin, /http\.allowed/);
	assert.match(plugin, /api\.wordpress\.org/);
	assert.match(plugin, /Blocked in smoke/);
	assert.match(plugin, /'code'\s*=> 598/);

	const defaultPolicy = normalizeExternalHttpGuardrailPolicy();
	assert.equal(defaultPolicy.blockNetwork, false);
	assert.deepEqual(defaultPolicy.allowlistDomains, []);

	const allowlistPolicy = normalizeExternalHttpGuardrailPolicy({ allowlistDomains: ['API.WordPress.org'] });
	assert.equal(allowlistPolicy.blockNetwork, true);
	assert.deepEqual(allowlistPolicy.allowlistDomains, ['api.wordpress.org']);

	assert.equal(
		redactExternalHttpGuardrailUrl('https://user:secret@example.test/path?token=abc&x=1#frag'),
		'https://redacted:redacted@example.test/path?redacted=1'
	);

	const paths = installWordPressExternalHttpGuardrail(fixture, {
		allowlistDomains: ['example.test'],
		blockResponse: { code: 599, message: 'No network', body: 'blocked' },
	});
	assert.equal(paths.artifactRelativePath, DEFAULT_EXTERNAL_HTTP_GUARDRAIL_ARTIFACT_RELATIVE_PATH);
	assert.equal(paths.pluginPath, path.join(fixture, 'wp-content', 'mu-plugins', 'homeboy-external-http-guardrail.php'));
	assert.equal(fs.existsSync(paths.pluginPath), true);
	assert.equal(fs.existsSync(path.dirname(paths.artifactPath)), true);

	fs.writeFileSync(
		paths.artifactPath,
		[
			JSON.stringify({ event: 'http.allowed', request_id: 'one', data: { host: 'api.wordpress.org', url: 'https://api.wordpress.org/core/version-check/1.7/?token=secret', method: 'GET', blocked: false } }),
			JSON.stringify({ event: 'http.blocked', request_id: 'one', data: { host: 'tracker.example.test', url: 'https://tracker.example.test/pixel?email=a@example.test', method: 'POST', blocked: true } }),
			JSON.stringify({ event: 'http.blocked', request_id: 'two', data: { host: 'tracker.example.test', url: 'https://tracker.example.test/again?key=value', method: 'GET', blocked: true } }),
			'',
		].join('\n'),
		'utf8'
	);

	const entries = collectWordPressExternalHttpGuardrailEvents(fixture);
	assert.equal(entries.length, 3);
	assert.equal(entries[1].data.host, 'tracker.example.test');

	const parsed = parseWordPressExternalHttpGuardrailJsonl('{"event":"one"}\n\n{"event":"two"}\n');
	assert.deepEqual(parsed.map((entry) => entry.event), ['one', 'two']);

	const summary = summarizeWordPressExternalHttpGuardrailEvents(entries);
	assert.equal(summary.event_count, 3);
	assert.equal(summary.allowed_count, 1);
	assert.equal(summary.blocked_count, 2);
	assert.deepEqual(summary.hosts[0], {
		host: 'tracker.example.test',
		count: 2,
		allowed: 0,
		blocked: 2,
	});
	assert.equal(summary.samples[0].url, 'https://api.wordpress.org/core/version-check/1.7/?redacted=1');
	assert.equal(summary.samples[1].blocked, true);

	assert.throws(
		() => parseWordPressExternalHttpGuardrailJsonl('{"event":"ok"}\nnot-json'),
		/Invalid WordPress external HTTP guardrail JSONL at line 2/
	);

	assert.throws(
		() => resolveExternalHttpGuardrailPaths(fixture, { artifactRelativePath: '../outside.jsonl' }),
		/must stay inside/
	);

	uninstallWordPressExternalHttpGuardrail(fixture);
	assert.equal(fs.existsSync(paths.pluginPath), false);
	assert.equal(fs.existsSync(paths.artifactPath), true);

	uninstallWordPressExternalHttpGuardrail(fixture, { removeArtifact: true });
	assert.equal(fs.existsSync(paths.artifactPath), false);

	console.log('WordPress external HTTP guardrail smoke passed.');
} finally {
	fs.rmSync(fixture, { recursive: true, force: true });
}
