#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { capture, normalizeProviderPlugin, requireRepo, run, splitCsv } = require('./lib/common.cjs');

function main() {
  const printPlan = process.argv.includes('--print-plan');
  const entries = dependencyEntries(process.env);
  const plan = resolvePlan(entries, printPlan);
  if (printPlan) {
    process.stdout.write(`${JSON.stringify(plan, null, 2)}\n`);
    return;
  }
  for (const item of plan) {
    fs.rmSync(item.target, { recursive: true, force: true });
    process.stdout.write(`Checking out validation dependency ${item.repo}@${item.ref} into ${item.target}\n`);
    run('gh', ['repo', 'clone', item.repo, item.target, '--', '--depth=1']);
    run('git', ['-C', item.target, 'fetch', '--depth=1', 'origin', item.ref]);
    run('git', ['-C', item.target, 'checkout', '--quiet', 'FETCH_HEAD']);
  }
}

function dependencyEntries(env) {
  const runtimeId = env.AGENT_RUNTIME || 'wp-codebox';
  if (runtimeId !== 'wp-codebox') {
    throw new Error(`Unsupported agent_runtime: ${runtimeId}. Only wp-codebox is currently supported.`);
  }
  const entries = [`Automattic/wp-codebox@${env.AGENT_RUNTIME_REF || 'main'}`];
  const providerPlugin = normalizeProviderPlugin(env.PROVIDER_PLUGIN || '{}', env.PROVIDER || 'openai', true);
  if (env.INCLUDE_AGENT_RUNTIME_DEPENDENCIES === 'true') {
    entries.push(`Automattic/agents-api@${env.AGENTS_API_REF || 'main'}`);
    entries.push(`Extra-Chill/data-machine@${env.DATA_MACHINE_REF || 'main'}`);
    entries.push(`Extra-Chill/data-machine-code@${env.DATA_MACHINE_CODE_REF || 'main'}`);
    if ((env.PROVIDER || 'openai') === 'openai' && !providerPlugin.repo) {
      entries.push(`WordPress/ai-provider-for-openai@${env.OPENAI_PROVIDER_REF || 'trunk'}`);
    }
    if (providerPlugin.repo) {
      entries.push(providerPlugin.ref ? `${providerPlugin.repo}@${providerPlugin.ref}` : providerPlugin.repo);
    }
  }
  entries.push(...splitCsv(env.VALIDATION_DEPENDENCIES || ''));
  return entries;
}

function resolvePlan(entries, offline) {
  const seen = new Set();
  const plan = [];
  for (const rawEntry of entries) {
    const entry = rawEntry.trim();
    if (!entry) {
      continue;
    }
    const atIndex = entry.lastIndexOf('@');
    const repo = atIndex === -1 ? entry : entry.slice(0, atIndex);
    let ref = atIndex === -1 ? '' : entry.slice(atIndex + 1);
    requireRepo(repo, 'validation_dependencies entries');
    if (atIndex !== -1 && !ref) {
      throw new Error(`validation_dependencies entries must include a non-empty ref after @: ${entry}`);
    }
    if (!ref) {
      if (offline) {
        ref = '<default-branch>';
      } else {
        ref = capture('gh', ['repo', 'view', repo, '--json', 'defaultBranchRef', '--jq', '.defaultBranchRef.name']);
        if (!ref) {
          throw new Error(`Could not resolve default branch for validation dependency: ${repo}`);
        }
      }
    }
    const key = `${repo}@${ref}`;
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    plan.push({ repo, ref, target: path.join('.ci', repo.split('/')[1]) });
  }
  return plan;
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}

module.exports = { dependencyEntries, resolvePlan };
