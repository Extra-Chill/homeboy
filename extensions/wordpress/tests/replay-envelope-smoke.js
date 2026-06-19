'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	redact,
	sha256Json,
	artifactReferences,
	hashArtifactReferences,
	buildEpisodeRows,
	writeEpisodeJsonl,
	buildSealedEnvelope,
} = require('../scripts/agent/replay-envelope');

const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-replay-envelope-'));

try {
	const resultsPath = path.join(tmpRoot, 'results.json');
	const transcriptPath = path.join(tmpRoot, 'transcript.jsonl');
	const episodePath = path.join(tmpRoot, 'episode.jsonl');
	fs.writeFileSync(resultsPath, '{}\n');
	fs.writeFileSync(transcriptPath, '{"role":"user","content":"Run it."}\n');

	const scenario = {
		id: 'portable-runtime-failure',
		artifacts: {
			transcript_json: { path: transcriptPath, kind: 'jsonl' },
		},
		metadata: {
			provider: 'example-provider',
			model: 'example-model',
			job_status: 'failed',
			api_token: 'must-not-leak',
			tool_audit_events: [
				{
					tool_name: 'generic/search',
					parameters_sha256: 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
					success: true,
					result_status: 'success',
				},
			],
		},
	};
	const config = {
		provider: 'example-provider',
		model: 'example-model',
		prompt: 'Reproduce the failure.',
		secret: 'must-not-leak',
	};

	assert.deepEqual(redact({ nested: { github_token: 'must-not-leak', safe: 'visible' } }), {
		nested: { github_token: '[redacted]', safe: 'visible' },
	});
	assert.equal(sha256Json({ b: 2, a: 1 }), sha256Json({ a: 1, b: 2 }));

	const references = artifactReferences(scenario, config, 'bundle.json', 'episode.jsonl');
	assert.equal(references.some((reference) => reference.name === 'transcript_json'), true);
	assert.equal(references.some((reference) => reference.name === 'episode_jsonl' && reference.required === true), true);

	const integrity = hashArtifactReferences(references, resultsPath);
	assert.equal(integrity.hashes.transcript_json.bytes, fs.statSync(transcriptPath).size);
	assert.equal(integrity.issues.some((issue) => issue.name === 'episode_jsonl'), true);

	const episodeRows = buildEpisodeRows(scenario, config);
	assert.equal(episodeRows.length, 2);
	assert.equal(episodeRows[0].row_type, 'action');
	assert.equal(episodeRows[0].action_name, 'generic/search');
	assert.equal(episodeRows[1].row_type, 'grader');
	assert.equal(JSON.stringify(episodeRows).includes('must-not-leak'), false);

	const writtenRows = writeEpisodeJsonl(episodePath, scenario, config);
	assert.equal(writtenRows.length, 2);
	assert.equal(fs.readFileSync(episodePath, 'utf8').trim().split('\n').length, 2);

	const completeReferences = artifactReferences(scenario, config, 'bundle.json', episodePath);
	const completeIntegrity = hashArtifactReferences(completeReferences, resultsPath);
	const envelope = buildSealedEnvelope({}, scenario, config, 'bundle.json', completeIntegrity, episodePath, writtenRows, {
		env: {
			GITHUB_REPOSITORY: 'Extra-Chill/homeboy-extensions',
			GITHUB_RUN_ID: '123',
			GITHUB_SHA: 'abc123',
		},
		projections: () => ({ runtime_projection: { id: 'example-runtime' } }),
	});

	assert.equal(envelope.status, 'ready_for_replay');
	assert.equal(envelope.runner.workflow_run_url, 'https://github.com/Extra-Chill/homeboy-extensions/actions/runs/123');
	assert.equal(envelope.runtime_projection.id, 'example-runtime');
	assert.equal(JSON.stringify(envelope).includes('must-not-leak'), false);
} finally {
	fs.rmSync(tmpRoot, { recursive: true, force: true });
}

process.stdout.write('Replay envelope smoke passed\n');
