'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { execFileSync, spawnSync } = require('node:child_process');

function trim(value) {
  return String(value || '').trim();
}

function parseJsonInput(name, value, expectedType, fallback) {
  const raw = trim(value);
  const parsed = raw === '' ? fallback : JSON.parse(raw);
  if (expectedType && jsonType(parsed) !== expectedType) {
    throw new Error(`Invalid ${name}: expected JSON ${expectedType}, got ${jsonType(parsed)}.`);
  }
  return parsed;
}

function jsonType(value) {
  if (Array.isArray(value)) {
    return 'array';
  }
  if (value === null) {
    return 'null';
  }
  return typeof value;
}

function requireRepo(value, name = 'repo') {
  if (!/^[^/\s]+\/[^/\s]+$/.test(value || '')) {
    throw new Error(`${name} must be OWNER/REPO: ${value || ''}`);
  }
  return value;
}

function splitCsv(value) {
  return String(value || '')
    .split(',')
    .map((entry) => entry.trim())
    .filter(Boolean);
}

function normalizeProviderPlugin(rawInput, provider, includeCredentials) {
  const raw = trim(rawInput);
  const providerPlugin = raw === '' ? {} : JSON.parse(raw);
  if (!providerPlugin || Array.isArray(providerPlugin) || typeof providerPlugin !== 'object') {
    throw new Error('provider_plugin must be a JSON object');
  }

  const normalized = {
    repo: jqAlternative(providerPlugin.repo, ''),
    ref: jqAlternative(providerPlugin.ref, ''),
    path: jqAlternative(providerPlugin.path, '.'),
    register_function: jqAlternative(providerPlugin.register_function, ''),
  };

  if (includeCredentials) {
    const credentials = jqAlternative(
      providerPlugin.credentials,
      provider === 'openai' ? { connectors_ai_openai_api_key: 'OPENAI_API_KEY' } : {}
    );
    if (!credentials || Array.isArray(credentials) || typeof credentials !== 'object') {
      throw new Error('provider_plugin.credentials must be a JSON object');
    }
    normalized.credentials = credentials;
  }

  return normalized;
}

function jqAlternative(value, fallback) {
  return value === undefined || value === null || value === false ? fallback : value;
}

function normalizeCommandList(name, value) {
  const entries = parseJsonInput(name, value, 'array', []);
  return entries.map((entry) => {
    if (typeof entry === 'string') {
      if (entry.length === 0) {
        throw new Error(`${name} entries require a non-empty command`);
      }
      return { command: entry, description: '' };
    }
    if (!entry || Array.isArray(entry) || typeof entry !== 'object') {
      throw new Error(`${name} entries must be strings or objects`);
    }
    const command = entry.command || '';
    const description = entry.description || '';
    if (typeof command !== 'string' || command.length === 0) {
      throw new Error(`${name} entries require a non-empty command`);
    }
    if (typeof description !== 'string') {
      throw new Error(`${name} descriptions must be strings`);
    }
    return { command, description };
  });
}

function normalizeContextRepositories(value) {
  return parseJsonInput('context_repositories', value, 'array', []).map((entry) => {
    if (!entry || Array.isArray(entry) || typeof entry !== 'object') {
      throw new Error('context_repositories entries must be objects');
    }
    requireRepo(entry.repo, 'context_repositories[].repo');
    const alias = entry.alias || entry.repo.split('/')[1];
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(alias)) {
      throw new Error('context_repositories[].alias must be a stable slug');
    }
    if ('ref' in entry && (typeof entry.ref !== 'string' || entry.ref.length === 0)) {
      throw new Error('context_repositories[].ref must be a non-empty string');
    }
    const paths = entry.paths || [];
    if (!Array.isArray(paths) || paths.some((item) => typeof item !== 'string' || item.length === 0)) {
      throw new Error('context_repositories[].paths must be an array of non-empty strings');
    }
    return { repo: entry.repo, ref: entry.ref || '', alias, paths };
  });
}

function normalizeWritablePaths(value) {
  const raw = trim(value);
  if (raw === '') {
    return [];
  }
  const entries = raw.startsWith('[') ? JSON.parse(raw) : splitCsv(raw);
  if (!Array.isArray(entries)) {
    throw new Error('writable_paths JSON must be an array');
  }
  entries.forEach((entry) => {
    if (typeof entry !== 'string' || entry.length === 0) {
      throw new Error('writable_paths entries must be non-empty strings');
    }
  });
  return entries;
}

function normalizePathSources(entries, workspace) {
  return entries.map((entry) => {
    if (entry && !Array.isArray(entry) && typeof entry === 'object' && typeof entry.source === 'string' && !path.isAbsolute(entry.source)) {
      return { ...entry, source: path.join(workspace, entry.source) };
    }
    return entry;
  });
}

function writeGithubOutput(values, outputPath = process.env.GITHUB_OUTPUT) {
  if (!outputPath) {
    return;
  }
  const lines = [];
  for (const [key, value] of Object.entries(values)) {
    const stringValue = String(value ?? '');
    if (stringValue.includes('\n')) {
      lines.push(`${key}<<EOF`, stringValue, 'EOF');
    } else {
      lines.push(`${key}=${stringValue}`);
    }
  }
  fs.appendFileSync(outputPath, `${lines.join('\n')}\n`);
}

function readJsonFile(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function findScenario(results, scenarioId) {
  const scenario = (results.scenarios || []).find((entry) => entry.id === scenarioId);
  if (!scenario) {
    throw new Error(`Scenario not found in results: ${scenarioId}`);
  }
  return scenario;
}

function getByPath(value, expression) {
  const normalized = String(expression || '').replace(/^\./, '');
  if (normalized === '') {
    return value;
  }
  return normalized.split('.').reduce((current, key) => {
    if (current === undefined || current === null) {
      return undefined;
    }
    return current[key];
  }, value);
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { stdio: 'inherit', ...options });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} failed with status ${result.status}`);
  }
}

function capture(command, args, options = {}) {
  return execFileSync(command, args, { encoding: 'utf8', ...options }).trim();
}

module.exports = {
  capture,
  findScenario,
  getByPath,
  normalizeCommandList,
  normalizeContextRepositories,
  normalizePathSources,
  normalizeProviderPlugin,
  normalizeWritablePaths,
  parseJsonInput,
  readJsonFile,
  requireRepo,
  run,
  splitCsv,
  trim,
  writeGithubOutput,
};
