'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	DEFAULT_ARTIFACT_RELATIVE_PATH,
	collectWordPressRequestProfiles,
	generateProfilerPlugin,
	installWordPressRequestProfiler,
	parseWordPressRequestProfileJsonl,
	resolveProfilerPaths,
	uninstallWordPressRequestProfiler,
} = require('../lib/request-profiler');

const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-profiler-'));

try {
	fs.mkdirSync(path.join(fixture, 'wp-content'), { recursive: true });

	const plugin = generateProfilerPlugin();
	assert.match(plugin, /Plugin Name: Homeboy Request Profiler/);
	assert.match(plugin, /admin_init/);
	assert.match(plugin, /pre_http_request/);
	assert.match(plugin, /homeboy-profile\.jsonl/);

	const paths = installWordPressRequestProfiler(fixture);
	assert.equal(paths.artifactRelativePath, DEFAULT_ARTIFACT_RELATIVE_PATH);
	assert.equal(paths.pluginPath, path.join(fixture, 'wp-content', 'mu-plugins', 'homeboy-request-profiler.php'));
	assert.equal(fs.existsSync(paths.pluginPath), true);
	assert.equal(fs.existsSync(path.dirname(paths.artifactPath)), true);

	fs.writeFileSync(
		paths.artifactPath,
		[
			JSON.stringify({ event: 'request.start', request_id: 'abc', t_ms: 0 }),
			JSON.stringify({ event: 'hook', request_id: 'abc', t_ms: 12.3, data: { hook: 'init' } }),
			'',
		].join('\n'),
		'utf8'
	);

	const entries = collectWordPressRequestProfiles(fixture);
	assert.equal(entries.length, 2);
	assert.equal(entries[1].data.hook, 'init');

	const parsed = parseWordPressRequestProfileJsonl('{"event":"one"}\n\n{"event":"two"}\n');
	assert.deepEqual(parsed.map((entry) => entry.event), ['one', 'two']);

	assert.throws(
		() => parseWordPressRequestProfileJsonl('{"event":"ok"}\nnot-json'),
		/Invalid WordPress request profile JSONL at line 2/
	);

	assert.throws(
		() => resolveProfilerPaths(fixture, { artifactRelativePath: '../outside.jsonl' }),
		/must stay inside/
	);

	uninstallWordPressRequestProfiler(fixture);
	assert.equal(fs.existsSync(paths.pluginPath), false);
	assert.equal(fs.existsSync(paths.artifactPath), true);

	uninstallWordPressRequestProfiler(fixture, { removeArtifact: true });
	assert.equal(fs.existsSync(paths.artifactPath), false);

	console.log('WordPress request profiler smoke passed.');
} finally {
	fs.rmSync(fixture, { recursive: true, force: true });
}
