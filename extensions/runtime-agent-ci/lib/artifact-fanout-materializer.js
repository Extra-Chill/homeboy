'use strict';

/* eslint-disable camelcase */

const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const ARTIFACT_FANOUT_CONFIG_SCHEMA = 'homeboy-extensions/artifact-fanout-materializer-config/v1';
const ARTIFACT_FANOUT_RESULT_SCHEMA = 'homeboy-extensions/artifact-fanout-materializer-result/v1';
const AGENT_TASK_PLAN_SCHEMA = 'homeboy/agent-task-plan/v1';

function materializeArtifactFanout(input = {}) {
  const config = normalizeObject(input.config || input);
  const source = normalizeObject(input.source || {});
  const artifact = source.artifact !== undefined ? source.artifact : artifactFromControllerInput(input.controller_input || input.controllerInput, config);
  const rawItems = input.items || itemsFromArtifact(artifact, config.item_path || config.itemPath || config.items_path || config.itemsPath || 'items');
  const items = normalizeItems(rawItems);
  if (items.length === 0 && config.requires_non_empty === true) {
    throw new Error(`artifact fanout source '${config.artifact || config.artifact_id || 'artifact'}' did not produce any items`);
  }
  const groups = groupItems(items, config.group_by || config.groupBy || config.group_key_path || config.groupKeyPath || 'group_key');
  const tasks = groups.map((group) => renderTaskRequest(group, config)).filter(Boolean);
  const plan = {
    schema: AGENT_TASK_PLAN_SCHEMA,
    plan_id: renderedText(config.plan_id || config.planId || config.fanout_id || config.fanoutId) || 'artifact-fanout',
    group_key: text(config.group_key || config.groupKey) || undefined,
    tasks,
    options: normalizeObject(config.options),
    metadata: stripUndefined({
      ...(normalizeObject(config.metadata)),
      source_artifact: text(config.artifact || config.artifact_id || config.artifactId),
      item_count: items.length,
      group_count: groups.length,
    }),
  };
  return stripUndefined({
    schema: ARTIFACT_FANOUT_RESULT_SCHEMA,
    config_schema: config.schema || ARTIFACT_FANOUT_CONFIG_SCHEMA,
    status: tasks.length > 0 ? 'planned' : 'no_items',
    item_count: items.length,
    group_count: groups.length,
    groups,
    plan,
  });
}

function runArtifactFanout(input = {}) {
  const result = materializeArtifactFanout(input);
  const config = normalizeObject(input.config || input);
  const mode = text(input.mode || config.mode) || 'plan';
  if (mode === 'plan' || result.plan.tasks.length === 0) {
    return result;
  }

  const homeboyBin = text(input.homeboy_bin || input.homeboyBin || config.homeboy_bin || config.homeboyBin || process.env.HOMEBOY_BIN) || 'homeboy';
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-artifact-fanout-'));
  const planPath = path.join(tmpRoot, 'plan.json');
  fs.writeFileSync(planPath, `${JSON.stringify(result.plan, null, 2)}\n`);

  const batchId = renderedText(input.batch_id || input.batchId || config.batch_id || config.batchId || result.plan.plan_id);
  const submitArgs = ['agent-task', 'fanout', 'submit-batch', '--input', planPath];
  if (batchId) {
    submitArgs.push('--batch-id', batchId);
  }
  const submitted = runHomeboy(homeboyBin, submitArgs, input.cwd || config.cwd);
  result.status = submitted.status === 0 ? 'submitted' : 'failed';
  result.plan_path = planPath;
  result.submit = commandResult(submitted);
  result.batch_id = jsonPath(submitted.parsed, 'batch.batch_id') || batchId;

  if (submitted.status !== 0 || mode === 'submit') {
    return result;
  }

  const maxDrains = positiveInteger(input.max_drains || input.maxDrains || config.max_drains || config.maxDrains) || result.plan.tasks.length;
  const drains = [];
  for (let index = 0; index < maxDrains; index += 1) {
    const drained = runHomeboy(homeboyBin, ['agent-task', 'run-next'], input.cwd || config.cwd);
    drains.push(commandResult(drained));
    if (drained.status !== 0) {
      break;
    }
  }
  result.drains = drains;
  if (result.batch_id) {
    const status = runHomeboy(homeboyBin, ['agent-task', 'fanout', 'status', result.batch_id], input.cwd || config.cwd);
    result.batch_status = commandResult(status);
    result.status = batchTerminalStatus(status.parsed) || result.status;
    const artifacts = runHomeboy(homeboyBin, ['agent-task', 'fanout', 'artifacts', result.batch_id], input.cwd || config.cwd);
    result.batch_artifacts = commandResult(artifacts);
  }
  return result;
}

function artifactFromControllerInput(input, config = {}) {
  const controller = normalizeObject(input);
  const artifactId = text(config.artifact || config.artifact_id || config.artifactId);
  if (!artifactId) {
    return undefined;
  }
  const lineages = Array.isArray(controller.controller?.task_lineage) ? controller.controller.task_lineage : [];
  for (const lineage of [...lineages].reverse()) {
    const artifact = artifactFromOutputs(lineage.outputs, artifactId);
    if (artifact !== undefined) {
      return artifact;
    }
  }
  return undefined;
}

function artifactFromOutputs(outputs, artifactId) {
  if (!outputs || typeof outputs !== 'object') {
    return undefined;
  }
  if (outputs.artifacts && Object.prototype.hasOwnProperty.call(outputs.artifacts, artifactId)) {
    return outputs.artifacts[artifactId];
  }
  if (outputs.typed_artifacts && Object.prototype.hasOwnProperty.call(outputs.typed_artifacts, artifactId)) {
    return outputs.typed_artifacts[artifactId];
  }
  const indexes = outputs.evidence_index?.entries || outputs.evidence_indexes?.flatMap((index) => index.entries || []) || [];
  for (const entry of indexes) {
    const match = (entry.typed_artifacts || []).find((artifact) => artifact.name === artifactId || artifact.output_key === artifactId);
    if (match) {
      return match.payload !== undefined ? match.payload : match;
    }
  }
  return undefined;
}

function itemsFromArtifact(artifact, itemPath) {
  if (Array.isArray(artifact)) {
    return artifact;
  }
  const configured = getPath(artifact, itemPath);
  if (Array.isArray(configured)) {
    return configured;
  }
  for (const key of ['items', 'groups', 'records', 'children']) {
    if (Array.isArray(artifact?.[key])) {
      return artifact[key];
    }
  }
  return artifact === undefined || artifact === null ? [] : [artifact];
}

function normalizeItems(items) {
  return Array.isArray(items) ? items.map((item, index) => (item && typeof item === 'object' ? { ...item, index } : { value: item, index })) : [];
}

function groupItems(items, groupBy) {
  const paths = Array.isArray(groupBy) ? groupBy : [groupBy];
  const map = new Map();
  for (const item of items) {
    const key = paths.map((entry) => text(getPath(item, entry))).filter(Boolean).join(':') || text(item.group_key) || text(item.id) || 'default';
    if (!map.has(key)) {
      map.set(key, { key, index: map.size, items: [] });
    }
    map.get(key).items.push(item);
  }
  return Array.from(map.values()).map((group) => ({
    ...group,
    item_count: group.items.length,
    item_ids: group.items.map((item) => text(item.id) || text(item.key) || String(item.index + 1)),
  }));
}

function renderTaskRequest(group, config = {}) {
  const template = config.task_request_template || config.taskRequestTemplate || {};
  const rendered = renderValue(template, { group, config, env: runtimeRenderEnv() });
  if (!rendered || typeof rendered !== 'object' || Array.isArray(rendered)) {
    return null;
  }
  return stripUndefined({
    schema: rendered.schema || 'homeboy/agent-task-request/v1',
    task_id: text(rendered.task_id || rendered.id) || stableTaskId(group, config),
    group_key: text(rendered.group_key || rendered.groupKey) || group.key,
    ...rendered,
    metadata: stripUndefined({
      ...(normalizeObject(rendered.metadata)),
      fanout_group_key: group.key,
      fanout_item_count: group.items.length,
    }),
  });
}

function runtimeRenderEnv() {
  const names = [
    'HOMEBOY_LOOP_ID',
    'HOMEBOY_LOOP_ACTION_ID',
    'HOMEBOY_LOOP_ACTION_DEDUPE_KEY',
    'HOMEBOY_AGENT_RUNTIME',
    'HOMEBOY_AGENT_RUNTIME_PROVIDER',
    'HOMEBOY_AGENT_RUNTIME_MODEL',
    'HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS',
    'HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT',
    'WP_CODEBOX_AGENT_RUNTIME_COMPONENT_PATHS',
    'HOMEBOY_WP_CODEBOX_CORE_MODULE',
    'HOMEBOY_EXTENSIONS_PATH',
  ];
  return Object.fromEntries(names.map((name) => [name, process.env[name] || '']));
}

function renderedText(value) {
  const raw = text(value);
  return raw ? text(renderString(raw, { env: runtimeRenderEnv(), config: {} })) : '';
}

function renderValue(value, context) {
  if (typeof value === 'string') {
    return renderString(value, context);
  }
  if (Array.isArray(value)) {
    return value.map((entry) => renderValue(entry, context));
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, renderValue(entry, context)]));
  }
  return value;
}

function renderString(value, context) {
  const exact = value.match(/^\{\{\s*([^}]+)\s*\}\}$/);
  if (exact) {
    return templateValue(exact[1], context);
  }
  return value.replace(/\{\{\s*([^}]+)\s*\}\}/g, (_match, expression) => stringifyTemplateValue(templateValue(expression, context)));
}

function templateValue(expression, context) {
  const key = expression.trim();
  if (key === 'group.item_ids') {
    return context.group.item_ids;
  }
  if (key === 'group.item_count') {
    return context.group.items.length;
  }
  return getPath(context, key);
}

function stringifyTemplateValue(value) {
  if (value === undefined || value === null) {
    return '';
  }
  return typeof value === 'string' ? value : JSON.stringify(value);
}

function runHomeboy(homeboyBin, args, cwd) {
  const run = spawnSync(homeboyBin, args, { cwd: cwd || process.cwd(), env: process.env, encoding: 'utf8' });
  const parsed = parseJson(run.stdout);
  return { status: run.status === null ? 1 : (run.status || 0), stdout: run.stdout || '', stderr: run.stderr || '', parsed, command: [homeboyBin, ...args] };
}

function commandResult(result) {
  return { command: result.command, status: result.status, stdout: result.stdout, stderr: result.stderr, parsed: result.parsed };
}

function batchTerminalStatus(value) {
  const state = text(jsonPath(value, 'batch.state'));
  if (state === 'succeeded') return 'succeeded';
  if (['failed', 'partial_failure', 'cancelled'].includes(state)) return 'failed';
  return '';
}

function parseJson(raw) {
  try { return JSON.parse(raw); } catch (_error) { return null; }
}

function getPath(value, expression) {
  if (!expression) return value;
  return String(expression).split('.').filter(Boolean).reduce((current, part) => current?.[part], value);
}

function jsonPath(value, expression) {
  return getPath(value, expression);
}

function stableTaskId(group, config) {
  const prefix = text(config.task_id_prefix || config.taskIdPrefix || config.plan_id || config.planId) || 'artifact-fanout';
  return `${prefix}-${group.key}`.replace(/[^a-zA-Z0-9._-]+/g, '-');
}

function positiveInteger(value) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
}

function normalizeObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function stripUndefined(value) {
  return Object.fromEntries(Object.entries(value || {}).filter(([, entry]) => entry !== undefined));
}

function text(value) {
  if (typeof value === 'string') return value.trim();
  if (typeof value === 'number' && Number.isFinite(value)) return String(value);
  return '';
}

module.exports = {
  AGENT_TASK_PLAN_SCHEMA,
  ARTIFACT_FANOUT_CONFIG_SCHEMA,
  ARTIFACT_FANOUT_RESULT_SCHEMA,
  artifactFromControllerInput,
  materializeArtifactFanout,
  runArtifactFanout,
};
