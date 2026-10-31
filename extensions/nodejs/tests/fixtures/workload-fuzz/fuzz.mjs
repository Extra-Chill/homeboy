// Tiny fuzz workload: parses a handful of inputs and reports a
// homeboy/fuzz-campaign/v1 file. `mode` (pass | fail) selects whether the
// empty-input case is still broken, so two runs can be compared.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const results = process.env.HOMEBOY_FUZZ_RESULTS_FILE;
const artifacts = process.env.HOMEBOY_FUZZ_ARTIFACTS_DIR;
const mode = existsSync('mode') ? readFileSync('mode', 'utf8').trim() : 'fail';
const seed = process.env.HOMEBOY_FUZZ_SEED || '1';
const parse = (input) => {
  if (input === '' && mode === 'fail') throw new Error('unexpected end of input');
  return JSON.parse(input === '' ? 'null' : input);
};

mkdirSync(join(artifacts, 'cases'), { recursive: true });
const inputs = { 'case-object': '{"a":1}', 'case-empty': '' };
const cases = [];
const findings = [];
const campaignArtifacts = [];
for (const [id, input] of Object.entries(inputs)) {
  let status = 'passed';
  let error;
  try {
    parse(input);
  } catch (caught) {
    status = 'failed';
    error = caught.message;
  }
  const artifactId = `${id}-input`;
  writeFileSync(join(artifacts, 'cases', `${id}.json`), JSON.stringify({ input }));
  campaignArtifacts.push({ id: artifactId, kind: 'case-input', path: `cases/${id}.json` });
  cases.push({ id, operation_id: 'parse', seed_id: seed, input, status, metadata: { artifact_id: artifactId } });
  if (error) {
    findings.push({
      id: `finding-${id}`,
      title: `parse throws: ${error}`,
      severity: 'high',
      status: 'open',
      operation_id: 'parse',
      case_id: id,
      fingerprint: 'throw:parse:empty',
    });
  }
}

writeFileSync(
  results,
  JSON.stringify({
    schema: 'homeboy/fuzz-campaign/v1',
    version: 1,
    id: `campaign-${process.env.HOMEBOY_FUZZ_RUN_ID || 'local'}`,
    safety_class: 'read_only',
    cases,
    findings,
    artifacts: campaignArtifacts,
    metadata: { status: findings.length ? 'failed' : 'passed', metrics: { inputs: cases.length } },
  }),
);
console.log(`fuzz fixture: ${cases.length} cases, ${findings.length} findings (mode=${mode})`);
