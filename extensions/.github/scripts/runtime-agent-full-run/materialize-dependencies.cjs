#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { capture, normalizeProviderPlugin, parseJsonInput, requireRepo, run, splitCsv } = require('./lib/common.cjs');
const { resolveRuntimeProvider, runtimeIdFromOptions } = require('../../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');

function main() {
  const printPlan = process.argv.includes('--print-plan');
  const entries = dependencyEntries(process.env);
  const plan = resolvePlan(entries, printPlan, { workspace: process.env.GITHUB_WORKSPACE || process.cwd() });
  if (printPlan) {
    process.stdout.write(`${JSON.stringify(plan, null, 2)}\n`);
    return;
  }
  for (const item of plan) {
    assertSafeDependencyTargetPath(item.targetPath, process.env.GITHUB_WORKSPACE || process.cwd());
    fs.rmSync(item.targetPath, { recursive: true, force: true });
    process.stdout.write(`Checking out validation dependency ${item.repo}@${item.ref} into ${item.target}\n`);
    run('gh', ['repo', 'clone', item.repo, item.targetPath, '--', '--depth=1']);
    run('git', ['-C', item.targetPath, 'fetch', '--depth=1', 'origin', item.ref]);
    run('git', ['-C', item.targetPath, 'checkout', '--quiet', 'FETCH_HEAD']);
  }
}

function dependencyEntries(env) {
  const runtimeId = runtimeIdFromOptions({}, env);
  const runtime = resolveRuntimeProvider(runtimeId, { env });
  const entries = runtime.checkout.repo ? [{ repo: runtime.checkout.repo, ref: runtime.checkout.ref, target: runtime.checkout.target }] : [];
  const providerPlugin = normalizeProviderPlugin(env.PROVIDER_PLUGIN || '{}', env.PROVIDER || '', true);
  const runtimeDependencies = runtimeDependencyEntries(env.RUNTIME_DEPENDENCIES || '');
  if (runtimeDependencies.length > 0) {
    entries.push(...runtimeDependencies);
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

function resolvePlan(entries, offline, options = {}) {
  const workspace = options.workspace || process.env.GITHUB_WORKSPACE || process.cwd();
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
    const safeTarget = resolveDependencyTarget(target || path.join('.ci', repo.split('/')[1]), workspace);
    plan.push({ repo, ref, target: safeTarget.target, targetPath: safeTarget.targetPath });
  }
  return plan;
}

function resolveDependencyTarget(rawTarget, workspace) {
  const target = String(rawTarget || '').trim();
  if (!target || target === '.' || target === '..' || path.isAbsolute(target) || path.win32.isAbsolute(target)) {
    throw new Error(`Dependency target must be a relative path under .ci/: ${target || '(empty)'}`);
  }

  const segments = target.split(/[\\/]+/);
  if (segments.some((segment) => segment === '' || segment === '.' || segment === '..')) {
    throw new Error(`Dependency target must not contain empty, current-directory, or parent-directory segments: ${target}`);
  }

  const normalized = path.normalize(target);
  const safeRoot = path.resolve(workspace, '.ci');
  const targetPath = path.resolve(workspace, normalized);
  const relativeToSafeRoot = path.relative(safeRoot, targetPath);
  // Dependency materialization deletes the target before cloning; keep that write bounded to .ci/*.
  if (relativeToSafeRoot === '' || relativeToSafeRoot.startsWith('..') || path.isAbsolute(relativeToSafeRoot)) {
    throw new Error(`Dependency target must resolve under .ci/: ${target}`);
  }

  return { target: normalized, targetPath };
}

function assertSafeDependencyTargetPath(targetPath, workspace) {
  const safeRoot = path.resolve(workspace, '.ci');
  fs.mkdirSync(safeRoot, { recursive: true });
  const workspaceRealPath = fs.realpathSync(path.resolve(workspace));
  const safeRootRealPath = fs.realpathSync(safeRoot);
  if (safeRootRealPath !== path.join(workspaceRealPath, '.ci')) {
    throw new Error(`Dependency safe root must be a workspace-owned directory: ${safeRoot}`);
  }

  let existingParent = targetPath;
  while (!fs.existsSync(existingParent)) {
    const parent = path.dirname(existingParent);
    if (parent === existingParent) {
      break;
    }
    existingParent = parent;
  }

  const parentRealPath = fs.realpathSync(existingParent);
  const relativeToSafeRoot = path.relative(safeRootRealPath, parentRealPath);
  if (relativeToSafeRoot.startsWith('..') || path.isAbsolute(relativeToSafeRoot)) {
    throw new Error(`Dependency target parent must resolve under .ci/: ${targetPath}`);
  }
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

module.exports = { assertSafeDependencyTargetPath, dependencyEntries, resolveDependencyTarget, resolvePlan };
