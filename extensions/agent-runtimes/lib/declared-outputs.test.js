'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	MAX_STRUCTURED_OUTPUT_BYTES,
	outputDeclarations,
	requiredOutputInstructions,
	structuredOutputsFromTexts,
} = require('./declared-outputs');

const identity = { runtime: 'demo', label: 'Demo' };

// Declarations: direct ones win over same-named inputs; structural_schema maps to json_schema.
{
	const declarations = outputDeclarations({
		inputs: { required_outputs: [{ name: 'review_form', required: false }, { name: 'extra', required: true }] },
		output_declarations: [{ name: 'review_form', required: true, structural_schema: { type: 'object' } }, { name: '' }],
	});
	assert.deepEqual(declarations, [
		{ name: 'extra', required: true },
		{ name: 'review_form', required: true, json_schema: { type: 'object' } },
	]);
}

// No declarations: no prompt suffix, no outputs.
assert.equal(requiredOutputInstructions({}), '');
assert.deepEqual(structuredOutputsFromTexts(['{"outputs":{"x":1}}'], {}, identity), {});

const request = { output_declarations: [{ name: 'review_form', required: true }, { name: 'notes' }] };
assert.match(requiredOutputInstructions(request), /^\n\nReturn one JSON object in your final answer with declared values under `outputs`/);

// Fenced JSON in the final answer.
{
	const text = 'All done.\n\n```json\n{"outputs":{"review_form":{"verdict":"ready"}}}\n```';
	const result = structuredOutputsFromTexts([text], request, identity);
	assert.deepEqual(result.outputs, { review_form: { verdict: 'ready' } });
	assert.deepEqual(result.missingRequiredOutputs, []);
	assert.deepEqual(result.diagnostics, []);
}

// Bare JSON answer and legacy top-level names.
{
	const result = structuredOutputsFromTexts(['{"review_form":{"verdict":"ready"},"notes":"n"}'], request, identity);
	assert.deepEqual(result.outputs, { review_form: { verdict: 'ready' }, notes: 'n' });
}

// The first text with a declared value wins; earlier texts are more authoritative.
{
	const result = structuredOutputsFromTexts(['no json here', '{"outputs":{"review_form":1}}', '{"outputs":{"review_form":2}}'], request, identity);
	assert.deepEqual(result.outputs, { review_form: 1 });
}

// Missing required output.
{
	const result = structuredOutputsFromTexts(['{"outputs":{"notes":"only notes"}}'], request, identity);
	assert.deepEqual(result.outputs, { notes: 'only notes' });
	assert.deepEqual(result.missingRequiredOutputs, ['review_form']);
	assert.equal(result.diagnostics[0].class, 'demo.required_outputs_missing');
	assert.match(result.diagnostics[0].message, /^Demo completed without required structured output\(s\): review_form\.$/);
}

// No JSON at all: every required output is missing.
{
	const result = structuredOutputsFromTexts(['plain text', ''], request, identity);
	assert.equal(Object.hasOwn(result, 'outputs'), false);
	assert.deepEqual(result.missingRequiredOutputs, ['review_form']);
}

// Oversized values are dropped and reported.
{
	const big = 'x'.repeat(MAX_STRUCTURED_OUTPUT_BYTES + 1);
	const result = structuredOutputsFromTexts([JSON.stringify({ outputs: { review_form: { ok: true }, notes: big } })], request, identity);
	assert.deepEqual(result.outputs, { review_form: { ok: true } });
	assert.equal(result.diagnostics.some((diagnostic) => diagnostic.class === 'demo.declared_outputs_oversized'), true);
}

process.stdout.write('Declared outputs contract passed\n');
