'use strict';

const path = require('node:path');

const PATH_MATERIALIZATION_PLAN_SCHEMA = 'homeboy/path-materialization-plan/v1';

function parsePathMaterializationPlan(rawInput, options = {}) {
  const raw = String(rawInput || '').trim();
  if (raw === '') {
    return {};
  }
  const plan = JSON.parse(raw);
  return normalizePathMaterializationPlan(plan, options);
}

function normalizePathMaterializationPlan(plan, options = {}) {
  if (!plainObject(plan)) {
    throw new Error('path_materialization_plan must be a JSON object');
  }
  if (Object.keys(plan).length === 0) {
    return {};
  }
  if (plan.schema !== PATH_MATERIALIZATION_PLAN_SCHEMA) {
    throw new Error(`path_materialization_plan.schema must be ${PATH_MATERIALIZATION_PLAN_SCHEMA}`);
  }

  const workspace = requiredString(options.workspace, 'workspace');
  const paths = plainObject(plan.paths) ? plan.paths : {};

  return stripUndefined({
    schema: PATH_MATERIALIZATION_PLAN_SCHEMA,
    runner_workspace_guest_checkout: optionalGuestPath(
      paths.runner_workspace_guest_checkout,
      'path_materialization_plan.paths.runner_workspace_guest_checkout'
    ),
    transcript_host_dir: optionalHostPath(
      paths.transcript_host_dir,
      workspace,
      'path_materialization_plan.paths.transcript_host_dir'
    ),
    transcript_dir: optionalGuestPath(
      paths.transcript_dir,
      'path_materialization_plan.paths.transcript_dir'
    ),
    runtime_mounts: normalizeMounts(plan.runtime_mounts || [], workspace),
  });
}

function normalizeMounts(mounts, workspace) {
  if (!Array.isArray(mounts)) {
    throw new Error('path_materialization_plan.runtime_mounts must be an array');
  }
  return mounts.map((mount, index) => {
    if (!plainObject(mount)) {
      throw new Error(`path_materialization_plan.runtime_mounts[${index}] must be an object`);
    }
    const source = requiredHostPath(mount.source, workspace, `path_materialization_plan.runtime_mounts[${index}].source`, { allowWorkspaceRoot: true });
    const target = requiredGuestPath(mount.target, `path_materialization_plan.runtime_mounts[${index}].target`);
    return stripUndefined({
      type: mount.type || 'directory',
      source,
      target,
      mode: mount.mode || 'readwrite',
      metadata: plainObject(mount.metadata) ? mount.metadata : undefined,
    });
  });
}

function requiredHostPath(value, workspace, name, options = {}) {
  const normalized = optionalHostPath(value, workspace, name, options);
  if (!normalized) {
    throw new Error(`${name} is required`);
  }
  return normalized;
}

function optionalHostPath(value, workspace, name, options = {}) {
  const raw = firstString(value);
  if (!raw) {
    return '';
  }
  if (path.win32.isAbsolute(raw)) {
    throw new Error(`${name} must be a workspace-relative path or absolute path under GITHUB_WORKSPACE: ${raw}`);
  }
  const relativeRaw = path.isAbsolute(raw) ? path.relative(path.resolve(workspace), raw) : raw;
  if ((relativeRaw === '' || relativeRaw === '.') && options.allowWorkspaceRoot === true) {
    return path.resolve(workspace);
  }
  if (relativeRaw === '' || relativeRaw === '.' || relativeRaw === '..' || relativeRaw.split(/[\\/]+/).some((segment) => segment === '' || segment === '..')) {
    throw new Error(`${name} must not contain empty or parent-directory segments: ${raw}`);
  }
  const resolved = path.resolve(workspace, raw);
  const relative = path.relative(path.resolve(workspace), resolved);
  if (relative === '..' || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
    throw new Error(`${name} must resolve under GITHUB_WORKSPACE: ${raw}`);
  }
  return resolved;
}

function requiredGuestPath(value, name) {
  const normalized = optionalGuestPath(value, name);
  if (!normalized) {
    throw new Error(`${name} is required`);
  }
  return normalized;
}

function optionalGuestPath(value, name) {
  const raw = firstString(value);
  if (!raw) {
    return '';
  }
  if (!raw.startsWith('/') || raw.startsWith('//') || raw.includes('\\')) {
    throw new Error(`${name} must be an absolute POSIX path: ${raw}`);
  }
  const normalized = path.posix.normalize(raw);
  if (normalized !== raw || normalized === '/' || normalized.split('/').includes('..')) {
    throw new Error(`${name} must be a normalized absolute POSIX path without parent-directory segments: ${raw}`);
  }
  return normalized;
}

function firstString(...values) {
  for (const value of values) {
    if (typeof value === 'string' && value.trim() !== '') {
      return value.trim();
    }
  }
  return '';
}

function requiredString(value, name) {
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`${name} is required`);
  }
  return value;
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function stripUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined && entry !== ''));
}

module.exports = {
  PATH_MATERIALIZATION_PLAN_SCHEMA,
  normalizePathMaterializationPlan,
  parsePathMaterializationPlan,
};
