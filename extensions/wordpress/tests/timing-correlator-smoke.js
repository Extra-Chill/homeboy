'use strict';

const assert = require('node:assert/strict');

const {
	correlateBrowserAndWordPressTimings,
	normalizeBrowserTiming,
	normalizeUrl,
	summarizeWordPressProfilerRows,
} = require('../lib/timing-correlator');

// --- normalizeUrl ----------------------------------------------------------

assert.equal(
	normalizeUrl('http://localhost:8881/wp-json/wp/v2/posts'),
	'/wp-json/wp/v2/posts',
	'origin should be stripped'
);
assert.equal(
	normalizeUrl('/wp-json/wp/v2/posts/'),
	'/wp-json/wp/v2/posts',
	'trailing slash should be stripped'
);
assert.equal(
	normalizeUrl('/wp-json/wp/v2/posts?_=12345&context=edit'),
	'/wp-json/wp/v2/posts?context=edit',
	'cache-busting `_=` query param should be dropped'
);
assert.equal(
	normalizeUrl('/wp-json/wp/v2/posts?b=2&a=1'),
	'/wp-json/wp/v2/posts?a=1&b=2',
	'query params should be sorted'
);
assert.equal(
	normalizeUrl('/wp-json/WP/v2/Posts'),
	'/wp-json/wp/v2/posts',
	'path should be lowercased by default'
);
assert.equal(
	normalizeUrl('/wp-json/WP/v2/Posts', { lowercasePath: false }),
	'/wp-json/WP/v2/Posts',
	'lowercasePath:false should preserve case'
);
assert.equal(
	normalizeUrl('http://example.test/wp-admin/edit.php?post_type=page#anchor'),
	'/wp-admin/edit.php?post_type=page',
	'fragments should be dropped'
);
assert.equal(normalizeUrl(''), '', 'empty input returns empty string');
assert.equal(normalizeUrl(null), '', 'non-string input returns empty string');

// Two URLs that look different but resolve to the same normalized form must
// be equal — this is what enables matching repeated endpoints.
assert.equal(
	normalizeUrl('http://localhost:8881/wp-json/wp/v2/posts/?_=1&context=edit'),
	normalizeUrl('https://other.host/wp-json/WP/v2/posts?context=edit'),
	'equivalent URLs should normalize to the same key'
);

// --- normalizeBrowserTiming ------------------------------------------------

const browserEntry = normalizeBrowserTiming({
	name: 'http://localhost:8881/wp-json/wp/v2/posts?_=42',
	startTime: 100,
	responseStart: 420,
	responseEnd: 480,
	duration: 380,
	initiatorType: 'fetch',
	method: 'get',
});

assert.equal(browserEntry.normalizedUrl, '/wp-json/wp/v2/posts');
assert.equal(browserEntry.method, 'GET');
assert.equal(browserEntry.ttfbMs, 320, 'ttfb derived from responseStart - startTime');
assert.equal(browserEntry.durationMs, 380);
assert.equal(browserEntry.initiatorType, 'fetch');

const browserNoDuration = normalizeBrowserTiming({
	url: '/wp-json/wp/v2/types',
	startTime: 0,
	responseEnd: 90,
});
assert.equal(browserNoDuration.durationMs, 90, 'duration derived from responseEnd - startTime when duration omitted');

assert.equal(normalizeBrowserTiming(null), null);
assert.equal(normalizeBrowserTiming({}), null);

// --- summarizeWordPressProfilerRows ----------------------------------------

// WordPress profiler `uri` mirrors `$_SERVER['REQUEST_URI']`, which usually
// includes the query string. Use realistic URIs so the correlator can prove it
// matches across origin / cache-buster / case differences.
const profilerRows = [
	{ event: 'request.start', request_id: 'r1', t_ms: 0, uri: '/wp-json/wp/v2/posts?context=edit&_=11', method: 'GET' },
	{ event: 'hook', request_id: 'r1', t_ms: 12.4, uri: '/wp-json/wp/v2/posts?context=edit&_=11', method: 'GET' },
	{ event: 'shutdown', request_id: 'r1', t_ms: 66, uri: '/wp-json/wp/v2/posts?context=edit&_=11', method: 'GET' },
	// Repeat of the same endpoint as a separate request — repeated endpoints
	// must be tracked as their own entry. Different cache-buster on purpose.
	{ event: 'request.start', request_id: 'r2', t_ms: 0, uri: '/wp-json/WP/v2/posts/?context=edit&_=22', method: 'GET' },
	{ event: 'shutdown', request_id: 'r2', t_ms: 90, uri: '/wp-json/WP/v2/posts/?context=edit&_=22', method: 'GET' },
	// Different endpoint.
	{ event: 'request.start', request_id: 'r3', t_ms: 0, uri: '/wp-json/wp/v2/types', method: 'GET' },
	{ event: 'shutdown', request_id: 'r3', t_ms: 33, uri: '/wp-json/wp/v2/types', method: 'GET' },
];

const summaries = summarizeWordPressProfilerRows(profilerRows);
assert.equal(summaries.length, 3, 'three distinct WordPress requests');
assert.deepEqual(
	summaries.map((s) => s.requestId),
	['r1', 'r2', 'r3'],
	'observation order is preserved'
);
assert.equal(summaries[0].durationMs, 66);
assert.equal(summaries[1].durationMs, 90);
assert.equal(summaries[2].durationMs, 33);
assert.equal(summaries[0].normalizedUri, '/wp-json/wp/v2/posts?context=edit', 'origin/case/cache-buster differences collapse via normalizeUrl');
// r1 and r2 hit the same logical endpoint via different surface URIs — they
// must normalize identically so the correlator can pair repeated requests.
assert.equal(summaries[0].normalizedUri, summaries[1].normalizedUri);

// summarizer without explicit shutdown should still produce a duration from maxTMs.
const partialSummaries = summarizeWordPressProfilerRows([
	{ event: 'request.start', request_id: 'r9', t_ms: 0, uri: '/x' },
	{ event: 'hook', request_id: 'r9', t_ms: 50, uri: '/x' },
]);
assert.equal(partialSummaries[0].durationMs, 50, 'falls back to max t_ms when shutdown missing');

assert.throws(() => summarizeWordPressProfilerRows('nope'), /must be an array/);

// --- correlateBrowserAndWordPressTimings -----------------------------------

const browserTimings = [
	// First /posts hit — matches r1.
	{
		name: 'http://localhost:8881/wp-json/wp/v2/posts?_=11&context=edit',
		startTime: 100,
		responseStart: 420,
		responseEnd: 480,
		duration: 380,
		initiatorType: 'fetch',
		phase: 'site-editor.boot',
	},
	// Repeat of /posts — must match r2, NOT r1 again.
	{
		name: 'http://localhost:8881/wp-json/wp/v2/posts?context=edit',
		startTime: 600,
		responseStart: 1030,
		responseEnd: 1100,
		duration: 500,
		initiatorType: 'fetch',
		phase: 'site-editor.boot',
	},
	// Different endpoint — matches r3.
	{
		name: '/wp-json/wp/v2/types',
		startTime: 700,
		responseStart: 760,
		responseEnd: 790,
		duration: 90,
		initiatorType: 'fetch',
		phase: 'site-editor.idle',
	},
	// Browser-only entry that should fall through to unmatchedBrowser.
	{
		name: '/wp-content/themes/twentytwentyfour/style.css',
		startTime: 50,
		responseStart: 60,
		responseEnd: 70,
		duration: 20,
		initiatorType: 'link',
	},
];

const result = correlateBrowserAndWordPressTimings({
	browserTimings,
	wordpressProfilerRows: profilerRows,
});

assert.equal(result.correlated.length, 3, 'three correlated rows');

// FIFO matching for repeated endpoints: the first /posts entry must match r1,
// the second must match r2 — never r1 twice.
const postsRows = result.correlated.filter((row) => row.normalizedUrl === '/wp-json/wp/v2/posts?context=edit');
assert.equal(postsRows.length, 2, 'both /posts requests correlated');
assert.equal(postsRows[0].wordpressRequestId, 'r1', 'first /posts pairs with r1');
assert.equal(postsRows[1].wordpressRequestId, 'r2', 'second /posts pairs with r2 — repeated endpoint must not collapse');
assert.notEqual(postsRows[0].wordpressRequestId, postsRows[1].wordpressRequestId);

// Delta math reflects the browser-vs-WordPress overhead the issue calls out.
const firstPosts = postsRows[0];
assert.equal(firstPosts.browserTtfbMs, 320);
assert.equal(firstPosts.wordpressDurationMs, 66);
assert.equal(firstPosts.transportDeltaMs, 254, 'TTFB - WP duration is the transport overhead');
assert.equal(firstPosts.totalDeltaMs, 314, 'total browser duration - WP duration');

// Phase grouping should aggregate the two boot rows together and the idle row separately.
const bootGroup = result.phaseGroups.find((g) => g.phase === 'site-editor.boot');
const idleGroup = result.phaseGroups.find((g) => g.phase === 'site-editor.idle');
assert.ok(bootGroup, 'boot phase group present');
assert.equal(bootGroup.count, 2);
assert.ok(idleGroup, 'idle phase group present');
assert.equal(idleGroup.count, 1);

// Unmatched buckets must surface entries that did not pair up.
assert.equal(result.unmatchedBrowser.length, 1);
assert.equal(result.unmatchedBrowser[0].normalizedUrl, '/wp-content/themes/twentytwentyfour/style.css');
assert.equal(result.unmatchedWordPress.length, 0);

// Surplus WordPress rows should land in unmatchedWordPress.
const surplusResult = correlateBrowserAndWordPressTimings({
	browserTimings: [browserTimings[0]],
	wordpressProfilerRows: profilerRows,
});
assert.equal(surplusResult.correlated.length, 1);
assert.equal(surplusResult.unmatchedWordPress.length, 2, 'unpaired WordPress requests are reported');

// Input validation.
assert.throws(
	() => correlateBrowserAndWordPressTimings(null),
	/requires an input object/
);
assert.throws(
	() => correlateBrowserAndWordPressTimings({ browserTimings: 'no', wordpressProfilerRows: [] }),
	/browserTimings must be an array/
);
assert.throws(
	() => correlateBrowserAndWordPressTimings({ browserTimings: [], wordpressProfilerRows: 'no' }),
	/wordpressProfilerRows must be an array/
);

console.log('Timing correlator smoke passed.');
