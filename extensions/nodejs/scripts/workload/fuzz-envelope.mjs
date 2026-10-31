#!/usr/bin/env node
// Convert a homeboy/fuzz-campaign/v1 file into the generic
// homeboy/workload-result/v1 envelope read by `homeboy workload` (#15840).
//
// Usage: node fuzz-envelope.mjs <campaign.json> <envelope.json> <runner-exit-code> [artifact-prefix]
//
// The campaign stays the envelope's opaque `payload`. Only the generic
// projection (status, findings, cases, metrics, artifacts) is derived here,
// so core never needs to know the fuzz schema.

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const [campaignPath, envelopePath, exitArg = '0', artifactPrefix = 'fuzz'] = process.argv.slice(2);
if (!campaignPath || !envelopePath) {
  console.error('usage: fuzz-envelope.mjs <campaign.json> <envelope.json> <runner-exit-code> [artifact-prefix]');
  process.exit(2);
}

const runnerExit = Number.parseInt(exitArg, 10) || 0;
const campaign = JSON.parse(readFileSync(campaignPath, 'utf8'));
const artifactRoot = process.env.HOMEBOY_WORKLOAD_ARTIFACT_DIR || '';
const lower = (value) => (typeof value === 'string' ? value.trim().toLowerCase() : '');
const object = (value) => (value && typeof value === 'object' && !Array.isArray(value) ? value : {});

// Same case-status sources and vocabulary core's fuzz totals read.
function caseStatus(fuzzCase) {
  const raw = lower(fuzzCase.status) || lower(object(fuzzCase.metadata).status) || lower(object(fuzzCase.observed).status);
  if (['passed', 'pass', 'ok', 'success'].includes(raw)) return 'pass';
  if (['failed', 'fail', 'error', 'errored'].includes(raw)) return 'fail';
  if (['skipped', 'skip'].includes(raw)) return 'skipped';
  return null;
}

const seedsById = new Map((campaign.seeds || []).map((seed) => [seed.id, seed]));

// Envelope artifacts: campaign artifacts whose relative path exists under the
// fuzz artifact directory, addressed relative to HOMEBOY_WORKLOAD_ARTIFACT_DIR.
const artifacts = [];
for (const artifact of campaign.artifacts || []) {
  const path = typeof artifact.path === 'string' ? artifact.path.trim() : '';
  if (!artifact.id || !path || path.startsWith('/') || path.split('/').includes('..')) continue;
  const relative = artifactPrefix ? `${artifactPrefix}/${path}` : path;
  if (artifactRoot && !existsSync(join(artifactRoot, relative))) continue;
  artifacts.push({ name: String(artifact.id), path: relative, ...(artifact.mime ? { mime: String(artifact.mime) } : {}) });
}
const artifactNames = new Set(artifacts.map((artifact) => artifact.name));

let unknownCases = 0;
const cases = (campaign.cases || [])
  .filter((fuzzCase) => typeof fuzzCase.id === 'string' && fuzzCase.id.trim())
  .map((fuzzCase) => {
    const metadata = object(fuzzCase.metadata);
    let status = caseStatus(fuzzCase);
    if (!status) {
      unknownCases += 1;
      status = 'skipped';
    }
    const seed = fuzzCase.seed ?? metadata.seed ?? seedsById.get(fuzzCase.seed_id)?.value ?? fuzzCase.seed_id;
    const artifactRef = [metadata.artifact_id, fuzzCase.artifact_id, fuzzCase.replay_id].find(
      (candidate) => typeof candidate === 'string' && artifactNames.has(candidate),
    );
    return {
      case_id: fuzzCase.id,
      ...(seed !== undefined && seed !== null && seed !== '' ? { seed: typeof seed === 'number' ? seed : String(seed) } : {}),
      ...(artifactRef ? { artifact_ref: artifactRef } : {}),
      status,
    };
  });

// Every finding is reported, whatever its triage status: the fuzz status
// travels in metadata, so a suppressed finding persists across runs rather
// than reading as "resolved".
const findings = (campaign.findings || []).map((finding) => ({
  tool: 'fuzz',
  ...(finding.operation_id || finding.target_id ? { rule: String(finding.operation_id || finding.target_id) } : {}),
  ...(finding.severity ? { severity: String(finding.severity) } : {}),
  message: String(finding.title || finding.id),
  fingerprint: String(finding.fingerprint || finding.id),
  metadata: {
    fuzz_status: finding.status,
    fuzz_finding_id: finding.id,
    ...(finding.case_id ? { case_id: finding.case_id } : {}),
  },
}));

const openFindings = (campaign.findings || []).filter((finding) => ['open', 'confirmed'].includes(lower(finding.status))).length;
const casesFailed = cases.filter((fuzzCase) => fuzzCase.status === 'fail').length;
const metadata = object(campaign.metadata);
const metadataStatus = lower(metadata.status);

let status;
if (['skipped', 'skip', 'unsupported', 'not_executed', 'not-executed'].includes(metadataStatus)) {
  status = 'skipped';
} else if (
  ['failed', 'errored', 'error'].includes(metadataStatus) ||
  metadata.success === false ||
  openFindings > 0 ||
  casesFailed > 0
) {
  status = 'fail';
} else {
  status = 'pass';
}
// The runner's exit code wins over a campaign that claims success.
if (runnerExit !== 0 && status !== 'fail') status = 'error';

const metrics = {
  cases_total: cases.length,
  cases_failed: casesFailed,
  cases_unknown: unknownCases,
  findings_total: findings.length,
  findings_open: openFindings,
};
for (const [name, value] of Object.entries(object(metadata.metrics))) {
  if (typeof value === 'number' && Number.isFinite(value) && !(name in metrics)) metrics[name] = value;
}

const workloadId = process.env.HOMEBOY_WORKLOAD_ID || undefined;
const envelope = {
  schema: 'homeboy/workload-result/v1',
  kind: process.env.HOMEBOY_WORKLOAD_KIND || 'fuzz',
  ...(workloadId ? { workload_id: workloadId } : {}),
  status,
  findings,
  cases,
  metrics,
  artifacts,
  payload: { schema: 'homeboy/fuzz-campaign/v1', ...campaign },
};
writeFileSync(envelopePath, `${JSON.stringify(envelope, null, 2)}\n`);
