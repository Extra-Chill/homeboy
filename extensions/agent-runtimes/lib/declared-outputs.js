'use strict';

/**
 * Declared outputs: the runtime-neutral contract for structured values a task
 * must return (for example `review_form`).
 *
 * A request declares outputs in `output_declarations` and/or
 * `inputs.required_outputs`. Every CLI runtime asks the model for one JSON
 * object under `outputs` in its final answer, then reads it back from that
 * final text. Core owns schema validation; runtimes only bound sizes and
 * report which required outputs are missing.
 */

const MAX_STRUCTURED_OUTPUT_BYTES = 64 * 1024;
const MAX_STRUCTURED_ANSWER_BYTES = MAX_STRUCTURED_OUTPUT_BYTES + 16 * 1024;

function arrayValue(value) {
	return Array.isArray(value) ? value : [];
}

/** Normalized declarations; direct declarations win over same-named inputs. */
function outputDeclarations(request = {}) {
	const direct = arrayValue(request.output_declarations);
	const directNames = new Set(direct.map((declaration) => declaration?.name).filter(Boolean));
	return [...arrayValue(request.inputs?.required_outputs).filter((declaration) => !directNames.has(declaration?.name)), ...direct]
		.filter((declaration) => declaration && typeof declaration === 'object'
			&& typeof declaration.name === 'string' && declaration.name.trim() !== '')
		.map((declaration) => {
			const { structural_schema: structuralSchema, ...normalized } = declaration;
			return {
				...normalized,
				name: declaration.name.trim(),
				required: declaration.required === true,
				...(declaration.json_schema === undefined && structuralSchema !== undefined
					? { json_schema: structuralSchema }
					: {}),
			};
		});
}

/** Prompt suffix asking for declared outputs; empty when nothing is declared. */
function requiredOutputInstructions(request = {}) {
	const declarations = outputDeclarations(request);
	if (declarations.length === 0) {
		return '';
	}
	return `\n\nReturn one JSON object in your final answer with declared values under \`outputs\`. Include every required declaration and any optional declaration you produced. Output declarations: ${JSON.stringify(declarations)}.`;
}

/** The first bounded JSON object in a final answer: fenced blocks first, then the whole text. */
function parseStructuredAnswer(text = '') {
	const candidates = [String(text).trim()];
	for (const match of String(text).matchAll(/```(?:json)?\s*([\s\S]*?)```/gi)) {
		candidates.unshift(match[1].trim());
	}
	for (const candidate of candidates) {
		if (Buffer.byteLength(candidate) > MAX_STRUCTURED_ANSWER_BYTES) {
			continue;
		}
		try {
			const parsed = JSON.parse(candidate);
			if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
				return parsed;
			}
		} catch {
			// Continue through the bounded final-answer candidates.
		}
	}
	return null;
}

function declaredOutputValues(envelope, declarations) {
	if (!envelope || typeof envelope !== 'object' || Array.isArray(envelope)) {
		return null;
	}
	// The canonical prompt contract is the `outputs` envelope.
	if (envelope.outputs && typeof envelope.outputs === 'object' && !Array.isArray(envelope.outputs)
		&& declarations.some((declaration) => Object.hasOwn(envelope.outputs, declaration.name))) {
		return envelope.outputs;
	}
	// Persisted pre-envelope recipes can only expose their explicitly declared names.
	const legacy = Object.fromEntries(declarations
		.filter((declaration) => Object.hasOwn(envelope, declaration.name))
		.map((declaration) => [declaration.name, envelope[declaration.name]]));
	return Object.keys(legacy).length > 0 ? legacy : null;
}

function boundedStructuredOutput(value) {
	try {
		return Buffer.byteLength(JSON.stringify(value)) <= MAX_STRUCTURED_OUTPUT_BYTES;
	} catch {
		return false;
	}
}

/**
 * Diagnostics for missing or oversized outputs. `runtime` names the class
 * prefix and the message subject, e.g. ('opencode', 'OpenCode').
 */
function outputDiagnostics(missing, oversized, { runtime = 'agent', label = 'The agent' } = {}) {
	return [
		...(missing.length > 0 ? [{
			class: `${runtime}.required_outputs_missing`,
			classification: 'provider',
			message: `${label} completed without required structured output(s): ${missing.map((declaration) => declaration.name).join(', ')}.`,
			data: { missing_outputs: missing.map((declaration) => declaration.name) },
		}] : []),
		...(oversized.length > 0 ? [{
			class: `${runtime}.declared_outputs_oversized`,
			classification: 'provider',
			message: `${label} emitted structured output(s) exceeding the size limit: ${oversized.join(', ')}.`,
			data: { oversized_outputs: oversized },
		}] : []),
	];
}

/**
 * Read declared outputs from final-answer texts, most authoritative first.
 * Returns `{ outputs?, missingRequiredOutputs, diagnostics }`.
 */
function structuredOutputsFromTexts(texts, request = {}, identity = {}) {
	const declarations = outputDeclarations(request);
	if (declarations.length === 0) {
		return {};
	}
	for (const text of texts) {
		const values = declaredOutputValues(parseStructuredAnswer(text), declarations);
		if (!values) {
			continue;
		}
		const outputs = {};
		const oversized = [];
		for (const declaration of declarations) {
			if (!Object.hasOwn(values, declaration.name)) {
				continue;
			}
			if (boundedStructuredOutput(values[declaration.name])) {
				// Core owns declaration-schema validation; retain bounded provider values.
				outputs[declaration.name] = values[declaration.name];
			} else {
				oversized.push(declaration.name);
			}
		}
		const missing = declarations.filter((declaration) => declaration.required && !Object.hasOwn(outputs, declaration.name));
		return {
			...(Object.keys(outputs).length > 0 ? { outputs } : {}),
			missingRequiredOutputs: missing.map((declaration) => declaration.name),
			diagnostics: outputDiagnostics(missing, oversized, identity),
		};
	}
	const missing = declarations.filter((declaration) => declaration.required);
	return {
		missingRequiredOutputs: missing.map((declaration) => declaration.name),
		diagnostics: outputDiagnostics(missing, [], identity),
	};
}

module.exports = {
	MAX_STRUCTURED_ANSWER_BYTES,
	MAX_STRUCTURED_OUTPUT_BYTES,
	outputDeclarations,
	outputDiagnostics,
	parseStructuredAnswer,
	requiredOutputInstructions,
	structuredOutputsFromTexts,
};
