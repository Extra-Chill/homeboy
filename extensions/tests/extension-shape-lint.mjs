#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';

const rootDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');

const extensionIds = new Set([
  'cloudflare-workers',
  'discord',
  'go',
  'managed-preview',
  'nodejs',
  'rust',
  'swift',
  'wordpress',
]);

const standardDiscoveryMarkers = {
  go: [{ all: ['go.mod'] }],
  nodejs: [{ all: ['package.json'] }],
  rust: [{ all: ['Cargo.toml'] }],
  swift: [{ any: ['Package.swift', 'project.yml', '*.xcodeproj', '*.xcworkspace'] }],
  wordpress: [{ all: ['*.php'] }, { all: ['style.css', 'functions.php'] }],
};

const rootManifest = readJson(path.join(rootDir, 'homeboy-extension-root.json')) || {};
const dependencyAdapterIndex = readJson(path.join(rootDir, 'dependency-adapters', 'index.json')) || {};
const dependencyAdapterIds = new Set(
  (Array.isArray(dependencyAdapterIndex.manifests) ? dependencyAdapterIndex.manifests : [])
    .map((manifest) => manifest.id)
    .filter((id) => typeof id === 'string'),
);

const allowedTopLevelDirs = new Set([
  '.git',
  '.github',
  '.claude',
  '.datamachine',
  'agent-runtimes',
  'datamachine-agent-ci',
  'defaults',
  'docs',
  'scripts',
  'tests',
  ...extensionIds,
  ...(Array.isArray(rootManifest.shared_assets) ? rootManifest.shared_assets : []),
]);

const failures = [];

function fail(message) {
  failures.push(message);
}

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch (error) {
    fail(`${path.relative(rootDir, file)} must parse as JSON: ${error.message}`);
    return null;
  }
}

function fileExists(file) {
  return fs.existsSync(file) && fs.statSync(file).isFile();
}

function assertRelativeFile(extensionId, relativePath, source) {
  if (!relativePath || path.isAbsolute(relativePath) || relativePath.includes('{{')) {
    return;
  }

  const target = path.join(rootDir, extensionId, relativePath);
  if (!fileExists(target)) {
    fail(`${extensionId}: ${source} references missing file ${relativePath}`);
  }
}

function walk(value, visit, keyPath = []) {
  if (Array.isArray(value)) {
    value.forEach((item, index) => walk(item, visit, [...keyPath, String(index)]));
    return;
  }

  if (!value || typeof value !== 'object') {
    return;
  }

  for (const [key, item] of Object.entries(value)) {
    visit(key, item, [...keyPath, key]);
    walk(item, visit, [...keyPath, key]);
  }
}

function extractExtensionPathCommands(command) {
  if (typeof command !== 'string') {
    return [];
  }

  const matches = [];
  const pattern = /\{\{extension_path\}\}\/([^"'\s]+)/g;
  let match;
  while ((match = pattern.exec(command)) !== null) {
    matches.push(match[1]);
  }
  return matches;
}

function isStringArray(value) {
  return Array.isArray(value) && value.every((item) => typeof item === 'string' && item.length > 0);
}

function hasDuplicates(values) {
  return new Set(values).size !== values.length;
}

function validateDiscoveryMarkers(extensionId, markers) {
  if (!Array.isArray(markers) || markers.length === 0) {
    fail(`${extensionId}: provides.discovery_markers must be a non-empty array`);
    return;
  }

  for (const [index, marker] of markers.entries()) {
    if (!marker || typeof marker !== 'object' || Array.isArray(marker)) {
      fail(`${extensionId}: discovery_markers.${index} must be an object`);
      continue;
    }
    if (Object.keys(marker).some((key) => key !== 'all' && key !== 'any')) {
      fail(`${extensionId}: discovery_markers.${index} may only contain all and any`);
    }
    if ((!Array.isArray(marker.all) || marker.all.length === 0) && (!Array.isArray(marker.any) || marker.any.length === 0)) {
      fail(`${extensionId}: discovery_markers.${index} must contain a non-empty all or any list`);
    }
    for (const field of ['all', 'any']) {
      if (marker[field] !== undefined && !isStringArray(marker[field])) {
        fail(`${extensionId}: discovery_markers.${index}.${field} must be an array of non-empty strings`);
      }
    }
  }
}

function validateComposition(extensionId, composition) {
  if (!composition || typeof composition !== 'object' || Array.isArray(composition)) {
    fail(`${extensionId}: composition must be an object`);
    return;
  }

  for (const field of ['includes', 'optional', 'conflicts']) {
    if (!isStringArray(composition[field])) {
      fail(`${extensionId}: composition.${field} must be an array of non-empty strings`);
    } else if (hasDuplicates(composition[field])) {
      fail(`${extensionId}: composition.${field} must not contain duplicates`);
    }
  }

  const includes = Array.isArray(composition.includes) ? composition.includes : [];
  for (const includedExtension of includes) {
    if (!extensionIds.has(includedExtension)) {
      fail(`${extensionId}: composition.includes references unknown extension ${includedExtension}`);
    }
  }

  for (const optionalAdapter of Array.isArray(composition.optional) ? composition.optional : []) {
    const adapterId = optionalAdapter.match(/^dependency-adapters\/([^/]+)$/)?.[1];
    if (!adapterId || !dependencyAdapterIds.has(adapterId)) {
      fail(`${extensionId}: composition.optional references unknown dependency adapter ${optionalAdapter}`);
    }
  }

  for (const conflictingExtension of Array.isArray(composition.conflicts) ? composition.conflicts : []) {
    if (conflictingExtension === extensionId || !extensionIds.has(conflictingExtension)) {
      fail(`${extensionId}: composition.conflicts references invalid extension ${conflictingExtension}`);
    } else if (includes.includes(conflictingExtension)) {
      fail(`${extensionId}: composition.conflicts must not include an extension from composition.includes`);
    }
  }

  const roles = composition.roles;
  if (!roles || typeof roles !== 'object' || Array.isArray(roles) || Object.keys(roles).length === 0) {
    fail(`${extensionId}: composition.roles must be a non-empty object`);
    return;
  }

  for (const [role, owner] of Object.entries(roles)) {
    if (!role) {
      fail(`${extensionId}: composition.roles must not contain an empty role`);
    }
    if (Array.isArray(owner)) {
      if (owner.length === 0 || !isStringArray(owner) || hasDuplicates(owner)) {
        fail(`${extensionId}: composition.roles.${role} must be a unique list of non-empty profile names`);
      }
    } else if (typeof owner !== 'string' || owner.length === 0 || ![extensionId, ...includes].includes(owner)) {
      fail(`${extensionId}: composition.roles.${role} must reference this extension or an included extension`);
    }
  }
}

function validateExtension(extensionId) {
  const extensionDir = path.join(rootDir, extensionId);
  const manifestPath = path.join(extensionDir, `${extensionId}.json`);
  const homeboyPath = path.join(extensionDir, 'homeboy.json');

  if (!fileExists(manifestPath)) {
    fail(`${extensionId}: missing ${extensionId}/${extensionId}.json`);
    return;
  }

  if (!fileExists(homeboyPath)) {
    fail(`${extensionId}: missing ${extensionId}/homeboy.json`);
    return;
  }

  const manifest = readJson(manifestPath);
  const metadata = readJson(homeboyPath);
  if (!manifest || !metadata) {
    return;
  }

  if (metadata.id !== extensionId) {
    fail(`${extensionId}: homeboy.json id must be ${extensionId}`);
  }

  if (manifest.id && manifest.id !== extensionId) {
    fail(`${extensionId}: manifest id must match directory id`);
  }

  if (Object.hasOwn(standardDiscoveryMarkers, extensionId)) {
    const markers = manifest.provides?.discovery_markers;
    validateDiscoveryMarkers(extensionId, markers);
    if (JSON.stringify(markers) !== JSON.stringify(standardDiscoveryMarkers[extensionId])) {
      fail(`${extensionId}: provides.discovery_markers must declare the standard project markers`);
    }
  }

  if (Object.hasOwn(standardDiscoveryMarkers, extensionId)) {
    validateComposition(extensionId, manifest.composition);
  }

  for (const [scriptName, scriptPath] of Object.entries(manifest.scripts || {})) {
    if (typeof scriptPath === 'string') {
      assertRelativeFile(extensionId, scriptPath, `scripts.${scriptName}`);
    }
  }

  walk(manifest, (key, value, keyPath) => {
    if (key === 'extension_script' && typeof value === 'string') {
      assertRelativeFile(extensionId, value, keyPath.join('.'));
    }

    if ((key === 'setup_command' || key === 'ready_check' || key === 'run_command' || key === 'command') && typeof value === 'string') {
      for (const relativePath of extractExtensionPathCommands(value)) {
        assertRelativeFile(extensionId, relativePath, keyPath.join('.'));
      }
    }
  });
}

for (const entry of fs.readdirSync(rootDir, { withFileTypes: true })) {
  if (entry.isDirectory() && !allowedTopLevelDirs.has(entry.name)) {
    fail(`undeclared top-level directory: ${entry.name}`);
  }
}

for (const extensionId of extensionIds) {
  validateExtension(extensionId);
}

if (failures.length > 0) {
  console.error('Extension shape lint failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log(`Extension shape lint passed for ${extensionIds.size} extensions.`);
