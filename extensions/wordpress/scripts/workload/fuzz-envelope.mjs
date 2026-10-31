#!/usr/bin/env node
// Convert a WordPress homeboy/fuzz-campaign/v1 file into the generic
// homeboy/workload-result/v1 envelope read by `homeboy workload`
// (Extra-Chill/homeboy#15840).
//
// Usage: node fuzz-envelope.mjs <campaign.json> <envelope.json> <runner-exit-code> [artifact-prefix]
//
// The campaign stays the envelope's opaque `payload`. Only the generic
// projection (status, findings, cases, metrics, artifacts) is derived here,
// so core never needs to know the WordPress fuzz schema.
//
// WordPress campaigns differ from the Node.js runner's: findings live on each
// case (budget breaches, runtime errors), not at the top level, and artifact
// paths sit under `artifact.path`.

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
const array = (value) => (Array.isArray(value) ? value : []);
const text = (value) => (typeof value === 'string' && value.trim() ? value.trim() : null);

// WordPress case vocabulary (normalizeFuzzStatus: passed, failed, errored,
// skipped) plus the generic aliases core's fuzz totals accept.
function caseStatus(fuzzCase) {
  const raw = lower(fuzzCase.status) || lower(object(fuzzCase.metadata).status);
  if (['passed', 'pass', 'ok', 'success', 'succeeded'].includes(raw)) return 'pass';
  if (['failed', 'fail', 'error', 'errored'].includes(raw)) return 'fail';
  if (['skipped', 'skip', 'unsupported'].includes(raw)) return 'skipped';
  return null;
}

// Envelope artifacts: every artifact with a safe relative path that exists
// under the fuzz artifact directory, addressed relative to
// HOMEBOY_WORKLOAD_ARTIFACT_DIR. The runner writes its required artifacts
// (case log, replay data, coverage, report) from the result envelope it embeds
// in campaign metadata; campaign-level artifacts drop their paths whenever the
// WP Codebox result carries its own artifact list, so read both.
const artifacts = [];
const artifactNames = new Set();
const artifactSources = [
  ...array(campaign.artifacts),
  ...array(object(object(campaign.metadata).fuzz_result_envelope).artifacts),
];
for (const artifact of artifactSources) {
  const name = text(artifact.id) || text(artifact.name);
  const contract = object(artifact.artifact);
  const path = text(contract.path) || text(artifact.path) || '';
  if (!name || artifactNames.has(name) || !path || path.startsWith('/') || path.split('/').includes('..')) continue;
  const relative = artifactPrefix ? `${artifactPrefix}/${path}` : path;
  if (artifactRoot && !existsSync(join(artifactRoot, relative))) continue;
  const mime = text(artifact.content_type) || text(object(artifact.metadata).content_type) || text(artifact.mime);
  artifactNames.add(name);
  artifacts.push({ name, path: relative, ...(mime ? { mime } : {}) });
}

// A case's stable identity across runs: what it exercised, not its generated id.
const caseSubject = (fuzzCase) =>
  text(fuzzCase.operation_id) || text(fuzzCase.target_id) || text(fuzzCase.surface_id) || text(fuzzCase.id) || 'case';

let unknownCases = 0;
const findings = [];
const cases = array(campaign.cases)
  .filter((fuzzCase) => text(fuzzCase.id))
  .map((fuzzCase) => {
    let status = caseStatus(fuzzCase);
    if (!status) {
      unknownCases += 1;
      status = 'skipped';
    }
    const subject = caseSubject(fuzzCase);
    for (const finding of array(fuzzCase.findings)) {
      const code = text(finding.code) || text(finding.kind) || 'finding';
      const metric = text(finding.metric);
      findings.push({
        tool: 'fuzz',
        rule: code,
        ...(text(finding.severity) ? { severity: finding.severity } : {}),
        message: text(finding.message) || `${code} on ${subject}`,
        fingerprint: [subject, code, metric].filter(Boolean).join(':'),
        metadata: {
          case_id: fuzzCase.id,
          ...(text(finding.kind) ? { kind: finding.kind } : {}),
          ...(metric ? { metric, actual: finding.actual, budget: finding.budget } : {}),
        },
      });
    }
    // A failed case with no structured finding is still a finding: core
    // matches findings by fingerprint, so a new failing operation regresses.
    if (status === 'fail' && array(fuzzCase.findings).length === 0) {
      findings.push({
        tool: 'fuzz',
        rule: 'case_failed',
        severity: 'error',
        message: text(fuzzCase.error) || `WordPress fuzz case ${fuzzCase.id} failed on ${subject}`,
        fingerprint: `${subject}:case_failed`,
        metadata: { case_id: fuzzCase.id },
      });
    }
    const seed = fuzzCase.seed ?? object(fuzzCase.metadata).seed;
    return {
      case_id: fuzzCase.id,
      ...(seed !== undefined && seed !== null && seed !== '' ? { seed: typeof seed === 'number' ? seed : String(seed) } : {}),
      status,
    };
  });

const casesFailed = cases.filter((fuzzCase) => fuzzCase.status === 'fail').length;
const metadata = object(campaign.metadata);
const metadataStatus = lower(metadata.status);

let status;
if (['skipped', 'skip', 'unsupported', 'not_executed', 'not-executed'].includes(metadataStatus)) {
  status = 'skipped';
} else if (['failed', 'errored', 'error'].includes(metadataStatus) || metadata.success === false || casesFailed > 0 || findings.length > 0) {
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
};
const coverage = object(object(campaign.coverage_summary).totals);
for (const [name, value] of Object.entries(coverage)) {
  if (typeof value === 'number' && Number.isFinite(value)) metrics[`coverage_${name}`] = value;
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
  payload: { ...campaign, schema: 'homeboy/fuzz-campaign/v1' },
};
writeFileSync(envelopePath, `${JSON.stringify(envelope, null, 2)}\n`);
