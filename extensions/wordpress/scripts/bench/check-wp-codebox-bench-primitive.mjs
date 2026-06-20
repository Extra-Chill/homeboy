#!/usr/bin/env node

/**
 * External dependencies
 */
import fs from 'node:fs';
import path from 'node:path';

const primitive = process.argv[2] || '';
const bin = process.argv[3] || '';

if (!primitive || !bin) {
	process.exit(0);
}

const primitiveNeedles = {
	'external-http-guardrail': [
		'wp_codebox_bench_run_external_http_guardrail_step',
		"$type === 'external-http-guardrail'",
	],
};

const needles = primitiveNeedles[primitive];
if (!needles) {
	process.exit(0);
}

function realpathIfExists(file) {
	try {
		return fs.realpathSync(file);
	} catch (_error) {
		return '';
	}
}

function packageRootCandidates(resolvedBin) {
	const candidates = new Set();
	let current = fs.statSync(resolvedBin).isDirectory() ? resolvedBin : path.dirname(resolvedBin);
	for (let index = 0; index < 8; index += 1) {
		if (fs.existsSync(path.join(current, 'package.json'))) {
			candidates.add(current);
		}
		const parent = path.dirname(current);
		if (parent === current) {
			break;
		}
		current = parent;
	}
	return [...candidates];
}

function sourceCandidates(packageRoot) {
	return [
		path.join(packageRoot, 'packages', 'runtime-playground', 'src', 'bench-command-handlers.ts'),
		path.join(packageRoot, 'packages', 'runtime-playground', 'dist', 'bench-command-handlers.js'),
		path.join(packageRoot, 'packages', 'runtime-playground', 'dist', 'index.js'),
		path.join(packageRoot, 'node_modules', '@automattic', 'wp-codebox-playground', 'dist', 'bench-command-handlers.js'),
		path.join(packageRoot, 'node_modules', '@automattic', 'wp-codebox-playground', 'dist', 'index.js'),
	];
}

const resolvedBin = realpathIfExists(bin);
if (!resolvedBin) {
	process.exit(0);
}

const inspected = [];
for (const root of packageRootCandidates(resolvedBin)) {
	for (const candidate of sourceCandidates(root)) {
		if (!fs.existsSync(candidate)) {
			continue;
		}
		inspected.push(candidate);
		const contents = fs.readFileSync(candidate, 'utf8');
		if (needles.every((needle) => contents.includes(needle))) {
			process.exit(0);
		}
	}
}

if (inspected.length === 0) {
	process.exit(0);
}

process.stderr.write([
	`WP Codebox ${primitive} bench primitive is not available in the selected runtime.`,
	`Selected wp-codebox binary: ${bin}`,
	`Resolved binary: ${resolvedBin}`,
	'Use a WP Codebox runtime built from a revision that includes the generic benchmark primitive, or set HOMEBOY_WP_CODEBOX_BIN / settings.wp_codebox_bin to that binary.',
	'Inspected runtime files:',
	...inspected.map((file) => `- ${file}`),
	'',
].join('\n'));
process.exit(1);
