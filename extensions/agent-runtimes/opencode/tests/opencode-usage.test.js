'use strict';

const assert = require('node:assert/strict');
const { parseOpenCodeUsage } = require('../lib/opencode-agent-task-executor');

const event = (id, tokens, cost) => JSON.stringify({
	type: 'step_finish',
	timestamp: 1767036064273,
	sessionID: 'ses_real_vocabulary_fixture',
	part: {
		id,
		sessionID: 'ses_real_vocabulary_fixture',
		messageID: 'msg_fixture',
		type: 'step-finish',
		reason: 'stop',
		...(cost === undefined ? {} : { cost }),
		tokens,
	},
});

const complete = parseOpenCodeUsage([
	event('prt_step_1', { input: 671, output: 8, reasoning: 0, cache: { read: 21415, write: 0 } }, 0.001),
	event('prt_step_2', { input: 21772, output: 110, reasoning: 0, cache: { read: 0, write: 0 } }, 0),
	// A duplicated capture of the first step must not charge it twice.
	event('prt_step_1', { input: 671, output: 8, reasoning: 0, cache: { read: 21415, write: 0 } }, 0.001),
].join('\n'));

assert.equal(complete.execution_id, 'ses_real_vocabulary_fixture');
assert.equal(complete.duplicate_events, 1);
assert.equal(complete.input_tokens, 22443);
assert.equal(complete.input_tokens_status, 'complete');
assert.equal(complete.output_tokens, 118);
assert.equal(complete.cache_read_tokens, 21415);
assert.equal(complete.cost_usd, 0.001);
assert.equal(complete.total_tokens, null);
assert.equal(complete.total_tokens_status, 'unknown');

const partial = parseOpenCodeUsage([
	event('prt_partial_1', { input: 10, output: 2, reasoning: 1, cache: { read: 0, write: 0 } }),
	event('prt_partial_2', { input: 20, output: 3, cache: { read: 4, write: 0 } }, 0),
	'not-json-provider-log-line',
].join('\n'));

assert.equal(partial.malformed_events, 1);
assert.equal(partial.input_tokens, null);
assert.equal(partial.input_tokens_status, 'partial');
assert.equal(partial.reasoning_tokens, null);
assert.equal(partial.reasoning_tokens_status, 'partial');
assert.equal(partial.cost_usd, null);
assert.equal(partial.cost_usd_status, 'partial');
assert.equal(partial.total_tokens_status, 'unknown');

const truncated = parseOpenCodeUsage(`${event('prt_truncated', { input: 1, output: 1, reasoning: 0, cache: { read: 0, write: 0 } }, 0)}\n${'x'.repeat(16 * 1024 * 1024)}`);
assert.equal(truncated.stream_truncated, true);
assert.equal(truncated.input_tokens_status, 'partial');

console.log('OpenCode usage adapter tests passed.');
