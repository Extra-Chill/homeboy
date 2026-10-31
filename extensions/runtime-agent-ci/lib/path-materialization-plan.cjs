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
  const projection = pathMaterializationProjection(plan);
  const projectedPathRemaps = normalizePathRemaps(projection.path_remaps, workspace, 'path_materialization_plan.projection.path_remaps');
  const canonicalPathRemaps = projectedPathRemaps.length > 0
    ? []
    : normalizeCanonicalEntryPathRemaps(plan.entries || [], workspace);
  const pathRemaps = projectedPathRemaps.length > 0 ? projectedPathRemaps : canonicalPathRemaps;
  const primaryWorkspaceRemotePath = firstString(
    projection.runner_workspace_guest_checkout,
    primaryWorkspaceRemotePathFromRemaps(pathRemaps),
    primaryWorkspaceRemotePathFromEntries(plan.entries || [])
  );

  return stripUndefined({
    schema: PATH_MATERIALIZATION_PLAN_SCHEMA,
    runner_workspace_guest_checkout: optionalGuestPath(
      primaryWorkspaceRemotePath,
      'path_materialization_plan.projection.runner_workspace_guest_checkout'
    ),
    transcript_host_dir: optionalHostPath(
      projection.transcript_host_dir,
      workspace,
      'path_materialization_plan.projection.transcript_host_dir'
    ),
    transcript_dir: optionalGuestPath(
      projection.transcript_dir,
      'path_materialization_plan.projection.transcript_dir'
    ),
    path_remaps: pathRemaps,
    runtime_mounts: runtimeMountsFromPathRemaps(pathRemaps),
  });
}

function pathMaterializationProjection(plan) {
  if (plainObject(plan.projection)) {
    return plan.projection;
  }
  return plan;
}

function normalizeCanonicalEntryPathRemaps(entries, workspace) {
  if (!Array.isArray(entries)) {
    throw new Error('path_materialization_plan.entries must be an array');
  }
  return entries.flatMap((entry, index) => {
    if (!plainObject(entry)) {
      throw new Error(`path_materialization_plan.entries[${index}] must be an object`);
    }
    if (!entry.local_path && !entry.remote_path) {
      return [];
    }
    if (!entry.local_path || !entry.remote_path) {
      return [];
    }
    return [normalizePathRemap(entry, workspace, `path_materialization_plan.entries[${index}]`)];
  });
}

function normalizePathRemaps(pathRemaps, workspace, name) {
  if (pathRemaps === undefined) {
    return [];
  }
  if (!Array.isArray(pathRemaps)) {
    throw new Error(`${name} must be an array`);
  }
  return pathRemaps.map((remap, index) => normalizePathRemap(remap, workspace, `${name}[${index}]`));
}

function normalizePathRemap(remap, workspace, name) {
  if (!plainObject(remap)) {
    throw new Error(`${name} must be an object`);
  }
  return stripUndefined({
    role: typeof remap.role === 'string' && remap.role.length > 0 ? remap.role : undefined,
    owner: typeof remap.owner === 'string' && remap.owner.length > 0 ? remap.owner : undefined,
    local_path: requiredHostPath(remap.local_path, workspace, `${name}.local_path`, { allowWorkspaceRoot: true }),
    remote_path: requiredGuestPath(remap.remote_path, `${name}.remote_path`),
  });
}

function runtimeMountsFromPathRemaps(pathRemaps) {
  return pathRemaps.map((remap) => stripUndefined({
    type: 'directory',
    source: remap.local_path,
    target: remap.remote_path,
    mode: 'readwrite',
    metadata: stripUndefined({
      role: remap.role,
      owner: remap.owner,
    }),
  }));
}

function primaryWorkspaceRemotePathFromRemaps(pathRemaps) {
  const primary = pathRemaps.find((remap) => remap.role === 'primary_workspace') || pathRemaps[0];
  return primary ? primary.remote_path : '';
}

function primaryWorkspaceRemotePathFromEntries(entries) {
  if (!Array.isArray(entries)) {
    return '';
  }
  const primary = entries.find((entry) => plainObject(entry) && entry.role === 'primary_workspace' && typeof entry.remote_path === 'string');
  return primary ? primary.remote_path : '';
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
  normalizePathRemaps,
  parsePathMaterializationPlan,
};
