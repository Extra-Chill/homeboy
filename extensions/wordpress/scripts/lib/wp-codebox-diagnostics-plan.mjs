const SUPPORTED_DIAGNOSTIC_COMMAND_SUFFIXES = [
	'wordpress.bench',
	'wordpress.phpunit',
	'wordpress.run-php',
	'wordpress.visual-compare',
];

const DEFAULT_DIAGNOSTIC_CAPTURE = ['queries', 'errors'];

export function applyWpCodeboxStepDiagnostics(recipe, options = {}) {
	const diagnostics = normalizeDiagnosticsPlan(options);
	if (!diagnostics) {
		return recipe;
	}

	const steps = recipe?.workflow?.steps;
	if (!Array.isArray(steps)) {
		return recipe;
	}

	for (const step of steps) {
		if (!step || typeof step !== 'object' || !supportsStepDiagnostics(step.command)) {
			continue;
		}

		step.diagnostics = mergeStepDiagnostics(step.diagnostics, diagnostics);
	}

	return recipe;
}

export function normalizeDiagnosticsPlan(options = {}) {
	const raw = options.diagnostics ?? options.commandDiagnostics ?? options.diagnosticsCapture ?? options.captureDiagnostics;
	if (raw === undefined || raw === null || raw === false) {
		return null;
	}

	if (raw === true) {
		return { capture: [...DEFAULT_DIAGNOSTIC_CAPTURE] };
	}

	if (Array.isArray(raw)) {
		return { capture: normalizeCaptureList(raw) };
	}

	if (typeof raw === 'object') {
		const capture = raw.capture ?? raw.captureTypes ?? raw.types;
		if (capture === true || capture === undefined) {
			return { ...raw, capture: [...DEFAULT_DIAGNOSTIC_CAPTURE] };
		}
		return { ...raw, capture: normalizeCaptureList(capture) };
	}

	return null;
}

export function supportsStepDiagnostics(command) {
	if (typeof command !== 'string' || !command.trim()) {
		return false;
	}

	return SUPPORTED_DIAGNOSTIC_COMMAND_SUFFIXES.some((suffix) => command === suffix || command.endsWith(`.${suffix}`));
}

function normalizeCaptureList(value) {
	const values = Array.isArray(value) ? value : [value];
	const capture = values
		.map((entry) => String(entry || '').trim())
		.filter(Boolean);

	return capture.length ? [...new Set(capture)] : [...DEFAULT_DIAGNOSTIC_CAPTURE];
}

function mergeStepDiagnostics(existing, diagnostics) {
	if (!existing || typeof existing !== 'object' || Array.isArray(existing)) {
		return diagnostics;
	}

	return {
		...diagnostics,
		...existing,
		capture: existing.capture === undefined ? diagnostics.capture : existing.capture,
	};
}
