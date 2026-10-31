#!/usr/bin/env node
'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');

function main() {
  const args = parseArgs(process.argv.slice(2));
  const input = args.input === '-' ? fs.readFileSync(0, 'utf8') : args.input || '';
  const provider = args.provider || '';
  const includeCredentials = args.includeCredentials === 'true';
  const providerPlugin = parseProviderPlugin(input);
  const normalized = {
    repo: jqAlternative(providerPlugin.repo, ''),
    ref: jqAlternative(providerPlugin.ref, ''),
    path: jqAlternative(providerPlugin.path, '.'),
    register_function: jqAlternative(providerPlugin.register_function, ''),
  };

  if (includeCredentials) {
    const providerSecretEnv = jqAlternative(providerPlugin.provider_secret_env, {});
    if (!providerSecretEnv || Array.isArray(providerSecretEnv) || typeof providerSecretEnv !== 'object') {
      throw new Error('provider_plugin.provider_secret_env must be a JSON object');
    }
    normalized.provider_secret_env = providerSecretEnv;
  }

  process.stdout.write(`${JSON.stringify(normalized)}\n`);
}

function parseArgs(args) {
  const parsed = {};
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (!arg.startsWith('--')) {
      continue;
    }
    const key = arg.slice(2);
    const next = args[index + 1];
    parsed[key] = next || '';
    index += 1;
  }
  return parsed;
}

function parseProviderPlugin(rawInput) {
  const trimmed = String(rawInput || '').trim();
  if (!trimmed) {
    return {};
  }

  const parsed = JSON.parse(trimmed);
  if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') {
    throw new Error('provider_plugin must be a JSON object');
  }
  return parsed;
}

function jqAlternative(value, fallback) {
  return value === undefined || value === null || value === false ? fallback : value;
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}
