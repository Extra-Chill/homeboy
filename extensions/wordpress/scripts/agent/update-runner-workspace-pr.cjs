#!/usr/bin/env node

const { readFileSync, writeFileSync, mkdtempSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function hasArg(name) {
  return process.argv.includes(name);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

function getPath(source, path) {
  const parts = String(path || '').split('.').filter(Boolean);
  let value = source;
  for (const part of parts) {
    if (!value || typeof value !== 'object' || !(part in value)) {
      return '';
    }
    value = value[part];
  }
  return value == null ? '' : value;
}

function scalar(value) {
  if (value == null) return '';
  if (typeof value === 'boolean') return value ? 'true' : 'false';
  if (typeof value === 'number') return String(value);
  if (typeof value === 'string') return value;
  return '';
}

function markdownTable(headers, rows) {
  if (!rows.length) return '_None recorded._';
  return [
    `| ${headers.join(' | ')} |`,
    `| ${headers.map(() => '---').join(' | ')} |`,
    ...rows.map((row) => `| ${row.map((cell) => String(cell).replace(/\r?\n/g, '<br>')).join(' | ')} |`),
  ].join('\n');
}

function render(template, values) {
  return String(template || '').replace(/\{([A-Za-z0-9_.-]+)\}/g, (_, key) => scalar(values[key]));
}

const resultsPath = argValue('--results');
const configPath = argValue('--config');
const scenarioId = argValue('--scenario');
const dryRun = hasArg('--dry-run');

if (!resultsPath || !configPath || !scenarioId) {
  fail('Usage: update-runner-workspace-pr.cjs --results <path> --config <path> --scenario <id> [--dry-run]');
}

const results = JSON.parse(readFileSync(resultsPath, 'utf8'));
const config = JSON.parse(readFileSync(configPath, 'utf8'));
const scenario = (results.scenarios || []).find((item) => item && item.id === scenarioId);
if (!scenario) {
  fail(`Scenario not found in results: ${scenarioId}`);
}

const metadata = scenario.metadata || {};
const publication = metadata.runner_workspace_publication || {};
const pullUrl = publication.url || publication.html_url || getPath(publication, 'result.html_url') || getPath(publication, 'result.url');
if (!pullUrl) {
  console.log('No runner workspace pull request found to update.');
  process.exit(0);
}

const artifactExport = config.artifact_export || {};
if (!artifactExport.pr_title_template || !artifactExport.pr_body_template) {
  console.log('No artifact PR title/body template configured.');
  process.exit(0);
}

const engineData = metadata.engine_data || {};
const runnerCapture = metadata.runner_workspace_capture || {};
const runnerStatus = runnerCapture.status || {};
const grade = scenario.grade || metadata.grade || engineData.grade || {};
const evalArtifact = metadata.eval_artifact || {};
const staticTemplateValues = artifactExport.pr_template_values || {};
const taskId = staticTemplateValues.task_id || config.task_id || config.workload_id || scenarioId;
const taskLabel = staticTemplateValues.task_label || config.task_label || config.workload_label || taskId;
const provider = staticTemplateValues.provider || config.provider || '';
const model = staticTemplateValues.model || config.model || '';
const modelLabel = staticTemplateValues.model_label || `${provider}/${model}`.replace(/^\//, '').replace(/\/$/, '');
const success = typeof scenario.success === 'boolean' ? scenario.success : metadata.success;
const resultLabel = success === false ? 'failed' : (metadata.success_status || (success === true ? 'success' : 'artifact'));
const workflowUrl = process.env.GITHUB_SERVER_URL && process.env.GITHUB_REPOSITORY && process.env.GITHUB_RUN_ID
  ? `${process.env.GITHUB_SERVER_URL.replace(/\/$/, '')}/${process.env.GITHUB_REPOSITORY}/actions/runs/${process.env.GITHUB_RUN_ID}`
  : '';

const resultRows = [
  ['Task', taskLabel],
  ['Task ID', taskId],
  ['Agent', config.agent_slug || 'agent'],
  ['Model', `${provider || 'provider'} / ${model || 'model'}`],
  ['Job', metadata.job_id || ''],
  ['Result', resultLabel],
  ['Success', scalar(success)],
  ['Reward', scalar(scenario.reward || metadata.reward)],
  ['Score', grade.score != null || grade.max_score != null ? `${scalar(grade.score)} / ${scalar(grade.max_score)}` : ''],
];

const checkRows = Array.isArray(grade.checks) ? grade.checks.map((check) => [
  check.id || '',
  check.passed ? 'yes' : 'no',
  scalar(check.score),
  scalar(check.max_score),
  check.message || '',
]) : [];

const generalRuleRows = Array.isArray(evalArtifact.general_rule_results)
  ? evalArtifact.general_rule_results.map((rule) => [
    rule.id || '',
    rule.status || '',
    rule.message || '',
  ])
  : [];

const toolRows = Array.isArray(engineData.tool_execution_summary) ? engineData.tool_execution_summary.map((tool) => [
  scalar(tool.turn_count),
  tool.tool_name || '',
  tool.success ? 'yes' : 'no',
]) : [];

const writtenPaths = [
  ...(Array.isArray(runnerStatus.files) ? runnerStatus.files : []),
  ...(Array.isArray(metadata.job_artifact_exports?.paths) ? metadata.job_artifact_exports.paths : []),
];
const linkRows = [];
if (workflowUrl) linkRows.push(['Workflow run', workflowUrl]);
for (const path of writtenPaths) {
  linkRows.push(['Artifact', `\`${path}\``]);
}

const values = {
  task_id: taskId,
  task_label: taskLabel,
  agent_slug: config.agent_slug || 'agent',
  provider,
  model,
  model_label: modelLabel,
  job_id: scalar(metadata.job_id),
  result_label: resultLabel,
  success: scalar(success),
  reward: scalar(scenario.reward || metadata.reward),
  grade_score: scalar(grade.score),
  grade_max_score: scalar(grade.max_score),
  workflow_run_url: workflowUrl,
  workspace_branch: runnerStatus.branch || getPath(publication, 'result.pull_request.head') || getPath(publication, 'result.head') || '',
  workspace_handle: runnerStatus.handle || runnerStatus.name || '',
  workspace_changed: runnerCapture.changed ? 'yes' : 'no',
  changed_file_count: scalar(runnerStatus.dirty || (Array.isArray(runnerStatus.files) ? runnerStatus.files.length : '')),
  result_table: markdownTable(['Field', 'Value'], resultRows),
  checks_table: markdownTable(['Check', 'Passed', 'Score', 'Max', 'Message'], checkRows),
  general_rules_table: markdownTable(['Rule', 'Status', 'Message'], generalRuleRows),
  tools_table: markdownTable(['Turn', 'Tool', 'Success'], toolRows),
  links_table: markdownTable(['Artifact', 'Location'], linkRows),
  paths: writtenPaths.length ? `- \`${writtenPaths.join('`\n- `')}\`` : '_None recorded._',
};

for (const [key, value] of Object.entries(staticTemplateValues)) {
  if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
    values[key] = scalar(value);
  }
}

const sources = { config, run: scenario, metadata, engine_data: engineData, artifact_result: metadata.job_artifact_exports || {} };
for (const [key, path] of Object.entries(artifactExport.pr_template_paths || {})) {
  const value = scalar(getPath(sources, path));
  if (value !== '') {
    values[key] = value;
  }
}

const title = render(artifactExport.pr_title_template, values);
const body = render(artifactExport.pr_body_template, values);

if (dryRun) {
  console.log(JSON.stringify({ pullUrl, title, body }, null, 2));
  process.exit(0);
}

const bodyPath = join(mkdtempSync(join(tmpdir(), 'homeboy-pr-body-')), 'body.md');
writeFileSync(bodyPath, body);
const result = spawnSync('gh', ['pr', 'edit', pullUrl, '--title', title, '--body-file', bodyPath], { stdio: 'inherit' });
process.exit(result.status || 0);
