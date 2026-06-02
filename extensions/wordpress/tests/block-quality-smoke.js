'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');

const {
	parseWordPressBlockQualityProbeOutput,
	probeWordPressBlockQuality,
	probeWordPressPostBlockQuality,
	wordpressBlockQualityProbeCode,
	wordpressPostBlockQualityProbeCode,
} = require('../lib/block-quality');

async function main() {
	const siteProbe = wordpressBlockQualityProbeCode({
		postTypes: ['page'],
		postStatuses: ['publish'],
		fallbackOptionNames: ['example_fallback_count'],
	});
	assert.match(siteProbe, /homeboy_wordpress_count_blocks/);
	assert.match(siteProbe, /fallback_option_names/);
	assert.doesNotMatch(siteProbe, /studio_bfb_unsupported_fallback_count/);

	const postProbe = wordpressPostBlockQualityProbeCode(123, { contentPreviewBytes: 80 });
	assert.match(postProbe, /stored_content_hash/);
	assert.match(postProbe, /stored_content_preview/);
	assert.throws(() => wordpressPostBlockQualityProbeCode('nope'), /postId must be a positive integer/);

	const parsed = parseWordPressBlockQualityProbeOutput(`notice\n${JSON.stringify({
		posts_seen: 2,
		posts_with_blocks: 1,
		total_blocks: 4,
		core_html_blocks: 1,
		serialized_block_comments: 4,
		fallback_count: 1,
		core_html_without_fallback: 0,
	})}`);
	assert.equal(parsed.total_blocks, 4);
	assert.equal(parsed.core_html_without_fallback, 0);
	assert.throws(() => parseWordPressBlockQualityProbeOutput('no json'), /did not emit JSON/);

	const calls = [];
	const siteQuality = await probeWordPressBlockQuality('/tmp/wp-site', {
		runCli: async (command, context) => {
			calls.push({ command, context });
			return {
				exitCode: 0,
				stdout: JSON.stringify({ posts_seen: 1, pages_seen: 1, total_blocks: 3 }),
				stderr: '',
			};
		},
	});
	assert.equal(siteQuality.total_blocks, 3);
	assert.equal(calls[0].context.sitePath, '/tmp/wp-site');
	assert.equal(calls[0].context.role, 'wordpress-block-quality-probe');
	assert.match(calls[0].command, /^eval '/);

	const postQuality = await probeWordPressPostBlockQuality('/tmp/wp-site', 55, {
		runCli: async () => ({
			exitCode: 0,
			stdout: JSON.stringify({ post_id: 55, stored_content_hash: 'abc', stored_content_bytes: 12 }),
			stderr: '',
		}),
	});
	assert.equal(postQuality.post_id, 55);

	await assert.rejects(
		() => probeWordPressBlockQuality('/tmp/wp-site', {
			runCli: async () => ({ exitCode: 1, stdout: 'stdout', stderr: 'stderr' }),
		}),
		/probe failed with exit code 1/
	);

	console.log('WordPress block quality smoke passed.');
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
