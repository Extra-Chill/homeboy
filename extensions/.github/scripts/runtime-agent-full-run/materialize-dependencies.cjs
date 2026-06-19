#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { capture, normalizeProviderPlugin, parseJsonInput, requireRepo, run, splitCsv } = require('./lib/common.cjs');
const { DEFAULT_RUNTIME_ID, resolveRuntimeProvider } = require('../../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');

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
  const runtimeId = env.RUNTIME_PROVIDER || DEFAULT_RUNTIME_ID;
  const runtime = resolveRuntimeProvider(runtimeId, { env });
  const entries = runtime.checkout.repo ? [{ repo: runtime.checkout.repo, ref: runtime.checkout.ref, target: runtime.checkout.target }] : [];
  const providerPlugin = normalizeProviderPlugin(env.PROVIDER_PLUGIN || '{}', env.PROVIDER || 'openai', true);
  const runtimeDependencies = runtimeDependencyEntries(env.RUNTIME_DEPENDENCIES || '');
  if (runtimeDependencies.length > 0) {
    entries.push(...runtimeDependencies);
  }
  if ((env.PROVIDER || 'openai') === 'openai' && !providerPlugin.repo) {
    entries.push(`WordPress/ai-provider-for-openai@${env.OPENAI_PROVIDER_REF || 'trunk'}`);
  }
  if (providerPlugin.repo) {
    entries.push(providerPlugin.ref ? `${providerPlugin.repo}@${providerPlugin.ref}` : providerPlugin.repo);
  }
  entries.push(...splitCsv(env.VALIDATION_DEPENDENCIES || ''));
  return entries;
}

function runtimeDependencyEntries(value) {
  if (!value || String(value).trim() === '') {
    return [];
  }
  if (String(value).trim().startsWith('[')) {
    return parseJsonInput('runtime_dependencies', value, 'array', []);
  }
  return splitCsv(value);
}

function resolvePlan(entries, offline) {
  const seen = new Set();
  const plan = [];
  for (const rawEntry of entries) {
    const entry = normalizeDependencyEntry(rawEntry);
    if (!entry.repo) {
      continue;
    }
    const { repo, target } = entry;
    let { ref } = entry;
    requireRepo(repo, 'validation_dependencies entries');
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
    plan.push({ repo, ref, target: target || path.join('.ci', repo.split('/')[1]) });
  }
  return plan;
}

function normalizeDependencyEntry(rawEntry) {
  if (rawEntry && !Array.isArray(rawEntry) && typeof rawEntry === 'object') {
    return {
      repo: rawEntry.repo || '',
      ref: rawEntry.ref || '',
      target: rawEntry.target || '',
    };
  }
  const entry = String(rawEntry || '').trim();
  if (!entry) {
    return { repo: '', ref: '', target: '' };
  }
  const atIndex = entry.lastIndexOf('@');
  const repo = atIndex === -1 ? entry : entry.slice(0, atIndex);
  const ref = atIndex === -1 ? '' : entry.slice(atIndex + 1);
  if (atIndex !== -1 && !ref) {
    throw new Error(`validation_dependencies entries must include a non-empty ref after @: ${entry}`);
  }
  return { repo, ref, target: '' };
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}

module.exports = { dependencyEntries, resolvePlan };
