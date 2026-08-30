/**
 * External dependencies
 */
import path from 'node:path';
import { realpathSync } from 'node:fs';

export function selectedScenarioIds(raw = '') {
  return [...new Set(String(raw).split(',').map((id) => id.trim()).filter(Boolean))];
}

export function rigWorkloadInputs(raw = '', selectedIds = [], pluginSlug = '', componentPath = '') {
  const selected = new Set(selectedIds);
  const workloads = [];
  const mounts = [];

  for (const source of String(raw).split(path.delimiter).map((entry) => entry.trim()).filter(Boolean)) {
    const filename = path.basename(source);
    const id = scenarioId(filename);
    if (selected.size > 0 && !selected.has(id)) {
      continue;
    }

    const componentRelativeFile = relativeComponentFile(source, componentPath);
    const relativeFile = componentRelativeFile || `.homeboy/bench-rig/${filename}`;
    workloads.push({
      id,
      source: 'rig',
      overridesDiscovered: true,
      run: [{ type: 'php', file: relativeFile }],
    });
    mounts.push({
      source,
      target: `/wordpress/wp-content/plugins/${pluginSlug}/${relativeFile}`,
      type: 'file',
      mode: 'readonly',
    });
  }

  return { workloads, mounts };
}

function relativeComponentFile(source, componentPath) {
  if (!componentPath) {
    return undefined;
  }

  const relative = path.relative(canonicalPath(componentPath), canonicalPath(source));
  if (!relative || path.isAbsolute(relative) || relative === '..' || relative.startsWith(`..${path.sep}`)) {
    return undefined;
  }

  return relative.split(path.sep).join('/');
}

function canonicalPath(value) {
  try {
    return realpathSync.native(value);
  } catch {
    return path.resolve(value);
  }
}

function scenarioId(filename) {
  return filename
    .replace(/\.[^.]+$/, '')
    .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
}
