#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const CONFIG_SCHEMA = 'homeboy-extensions/artifact-fanout-materializer-config/v1';
const BATCH_SCHEMA = 'homeboy-extensions/ArtifactFanoutBatch/v1';

try {
  const args = parseArgs(process.argv.slice(2));
  const configPath = args.config || process.env.HOMEBOY_ARTIFACT_FANOUT_CONFIG || '';
  if (!configPath) {
    throw new Error('Pass --config <path> or HOMEBOY_ARTIFACT_FANOUT_CONFIG.');
  }
  const config = readJson(resolvePath(configPath));
  if (config.schema && config.schema !== CONFIG_SCHEMA) {
    throw new Error(`Unsupported artifact fanout config schema: ${config.schema}`);
  }
  const actionInput = readJson(requiredEnv('HOMEBOY_LOOP_ACTION_INPUT'));
  const actionOutput = requiredEnv('HOMEBOY_LOOP_ACTION_OUTPUT');
  const artifactId = requiredString(config.artifact, 'config.artifact');
  const outputArtifact = requiredString(config.output_artifact || config.outputArtifact, 'config.output_artifact');
  const artifact = actionInput?.request?.inputs?.artifacts?.[artifactId]
    || actionInput?.request?.inputs?.[artifactId];
  const items = extractItems(artifact, config.item_path || config.itemPath || 'items');
  const groups = groupItems(items, normalizeStringArray(config.group_by || config.groupBy), config.requires_non_empty === true);
  const taskRequests = groups.map((group) => renderValue(config.task_request_template || {}, {
    group,
    config,
    env: process.env,
    action: {
      loop_id: process.env.HOMEBOY_LOOP_ID || actionInput.loop_id || '',
      action_id: process.env.HOMEBOY_LOOP_ACTION_ID || actionInput.action_id || '',
    },
  }));
  const batch = {
    schema: BATCH_SCHEMA,
    mode: config.mode || 'run',
    plan_id: renderString(config.plan_id || 'artifact-fanout', { env: process.env }),
    batch_id: renderString(config.batch_id || `${process.env.HOMEBOY_LOOP_ID || 'loop'}-${process.env.HOMEBOY_LOOP_ACTION_ID || 'action'}-${outputArtifact}`, { env: process.env }),
    source_artifact: artifactId,
    output_artifact: outputArtifact,
    item_count: items.length,
    group_count: groups.length,
    groups,
    task_requests: taskRequests,
  };
  writeJson(actionOutput, { artifacts: { [outputArtifact]: batch } });
} catch (error) {
  process.stderr.write(`${error?.stack || error?.message || String(error)}\n`);
  process.exitCode = 1;
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith('--')) continue;
    const key = arg.slice(2).replace(/-/g, '_');
    args[key] = argv[index + 1];
    index += 1;
  }
  return args;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function resolvePath(filePath) {
  return path.isAbsolute(filePath) ? filePath : path.resolve(process.cwd(), filePath);
}

function extractItems(artifact, itemPath) {
  const payload = artifact?.payload || artifact;
  const configured = getPath(payload, itemPath);
  const value = Array.isArray(configured) ? configured : Array.isArray(payload) ? payload : [];
  return value.map((item, index) => (item && typeof item === 'object' ? { ...item, index } : { value: item, index }));
}

function groupItems(items, groupBy, requiresNonEmpty) {
  if (requiresNonEmpty && items.length === 0) {
    throw new Error('Artifact fanout requires at least one item.');
  }
  const map = new Map();
  for (const item of items) {
    const key = groupBy.length > 0
      ? groupBy.map((field) => text(getPath(item, field))).filter(Boolean).join(':')
      : text(item.group_key || item.groupKey || item.id || item.index);
    const groupKey = key || 'default';
    if (!map.has(groupKey)) {
      map.set(groupKey, { key: groupKey, index: map.size, items: [] });
    }
    map.get(groupKey).items.push(item);
  }
  return Array.from(map.values()).map((group) => ({
    ...group,
    item_count: group.items.length,
    item_ids: group.items.map((item) => text(item.id || item.finding_id || item.packet_id || item.index)).filter(Boolean),
  }));
}

function renderValue(value, context) {
  if (typeof value === 'string') return renderString(value, context);
  if (Array.isArray(value)) return value.map((entry) => renderValue(entry, context));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, renderValue(entry, context)]));
  }
  return value;
}

function renderString(value, context) {
  return String(value || '').replace(/\{\{\s*([^}]+)\s*\}\}/g, (_match, expression) => {
    const resolved = getPath(context, expression.trim());
    if (resolved === undefined || resolved === null) return '';
    return typeof resolved === 'object' ? JSON.stringify(resolved) : String(resolved);
  });
}

function getPath(value, expression) {
  return String(expression || '').split('.').filter(Boolean).reduce((current, segment) => {
    if (current === undefined || current === null) return undefined;
    return current[segment];
  }, value);
}

function normalizeStringArray(value) {
  return Array.isArray(value) ? value.map(text).filter(Boolean) : [];
}

function text(value) {
  return String(value ?? '').trim();
}

function requiredString(value, name) {
  const result = text(value);
  if (!result) throw new Error(`${name} is required.`);
  return result;
}

function requiredEnv(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required.`);
  return value;
}
