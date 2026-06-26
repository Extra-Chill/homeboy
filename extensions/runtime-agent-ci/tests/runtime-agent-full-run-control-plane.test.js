'use strict';

const assert = require('node:assert/strict');

const {
  normalizeArtifactDownloads,
} = require('../../.github/scripts/runtime-agent-full-run/actions-artifact-downloads.cjs');
const {
  projectCallbackData,
} = require('../../.github/scripts/runtime-agent-full-run/project-callback-data.cjs');

assert.deepEqual(normalizeArtifactDownloads(JSON.stringify([
  { runId: '123', artifactName: 'payload' },
  { repo: 'Extra-Chill/other', run_id: '456', name: 'report', destination: 'artifacts/report' },
]), 'Extra-Chill/example'), [
  { repo: 'Extra-Chill/example', run_id: '123', name: 'payload', dir: '.ci/actions-artifacts/payload' },
  { repo: 'Extra-Chill/other', run_id: '456', name: 'report', dir: 'artifacts/report' },
]);

assert.throws(
  () => normalizeArtifactDownloads(JSON.stringify([{ name: 'payload' }]), 'Extra-Chill/example'),
  /actions_artifact_downloads\[0\]\.run_id is required/
);

assert.deepEqual(projectCallbackData({ CALLBACK_DATA: '{"source":"workflow","attempt":2}' }), {
  callback_data_json: '{"source":"workflow","attempt":2}',
});

assert.deepEqual(projectCallbackData({ CALLBACK_DATA: 'null' }), {
  callback_data_json: '{}',
});

assert.deepEqual(projectCallbackData({ CALLBACK_DATA: 'false' }), {
  callback_data_json: '{}',
});

assert.throws(
  () => projectCallbackData({ CALLBACK_DATA: '[]' }),
  /Invalid callback_data: expected JSON object/
);

process.stdout.write('Runtime agent full-run control-plane projection checks passed\n');
