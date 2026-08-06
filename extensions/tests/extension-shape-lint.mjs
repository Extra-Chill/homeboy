#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';

const rootDir = process.env.HOMEBOY_EXTENSION_ROOT
  ? path.resolve(process.env.HOMEBOY_EXTENSION_ROOT)
  : path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');

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

  if (!isStringArray(composition.includes)) {
    fail(`${extensionId}: composition.includes must be an array of non-empty strings`);
  } else if (hasDuplicates(composition.includes)) {
    fail(`${extensionId}: composition.includes must not contain duplicates`);
  }

  const includes = Array.isArray(composition.includes) ? composition.includes : [];
  for (const includedExtension of includes) {
    if (!extensionIds.has(includedExtension)) {
      fail(`${extensionId}: composition.includes references unknown extension ${includedExtension}`);
    }
  }

  for (const retired of ['optional', 'conflicts', 'roles']) {
    if (composition[retired] !== undefined) {
      fail(`${extensionId}: composition.${retired} is retired and read by nothing — remove it`);
    }
  }
}

function validateToolchainReadiness(extensionId, probes) {
  if (probes === undefined) {
    return;
  }

  if (!Array.isArray(probes)) {
    fail(`${extensionId}: toolchain_readiness must be an array`);
    return;
  }

  const ids = new Set();
  for (const [index, probe] of probes.entries()) {
    const source = `${extensionId}: toolchain_readiness.${index}`;
    if (!probe || typeof probe !== 'object' || Array.isArray(probe)) {
      fail(`${source} must be an object`);
      continue;
    }
    const allowed = new Set(['id', 'capabilities', 'program', 'args', 'repair_command', 'diagnostic_env']);
    for (const key of Object.keys(probe)) {
      if (!allowed.has(key)) {
        fail(`${source}.${key} is unsupported; readiness probes use program and args, not shell command strings`);
      }
    }
    if (typeof probe.id !== 'string' || probe.id.length === 0) {
      fail(`${source}.id must be a non-empty string`);
    } else if (ids.has(probe.id)) {
      fail(`${source}.id must be unique`);
    } else {
      ids.add(probe.id);
    }
    if (typeof probe.program !== 'string' || probe.program.length === 0) {
      fail(`${source}.program must be a non-empty executable name or path`);
    }
    if (probe.args !== undefined && !isStringArray(probe.args)) {
      fail(`${source}.args must be an array of non-empty literal arguments`);
    }
    if (probe.capabilities !== undefined && (!isStringArray(probe.capabilities) || hasDuplicates(probe.capabilities))) {
      fail(`${source}.capabilities must be unique non-empty strings`);
    }
    if (probe.repair_command !== undefined && (typeof probe.repair_command !== 'string' || probe.repair_command.length === 0)) {
      fail(`${source}.repair_command must be a non-empty diagnostic string`);
    }
    if (probe.diagnostic_env !== undefined && (!isStringArray(probe.diagnostic_env) || probe.diagnostic_env.some((name) => !/^[A-Z_][A-Z0-9_]*$/.test(name)))) {
      fail(`${source}.diagnostic_env must contain environment variable names`);
    }
  }
}

function validateRustToolchainReadiness(manifest) {
  const expected = [{
    id: 'cargo-lint-toolchain',
    capabilities: ['lint'],
    program: 'cargo',
    args: ['--version'],
    repair_command: 'rustup default stable',
    diagnostic_env: ['PATH', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN'],
  }, {
    id: 'cargo-fmt-lint-toolchain',
    capabilities: ['lint'],
    program: 'cargo',
    args: ['fmt', '--version'],
    repair_command: 'rustup default stable',
    diagnostic_env: ['PATH', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN'],
  }];

  if (JSON.stringify(manifest.toolchain_readiness) !== JSON.stringify(expected)) {
    fail('rust: toolchain_readiness must declare the Cargo lint toolchain probe');
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

  validateToolchainReadiness(extensionId, manifest.toolchain_readiness);

  if (extensionId === 'rust') {
    validateRustToolchainReadiness(manifest);
  }

  if (manifest.provides?.capabilities !== undefined) {
    fail(`${extensionId}: provides.capabilities is retired and read by nothing — remove it`);
  }

  for (const retired of ['validate', 'contract', 'crossref']) {
    if (manifest.scripts?.[retired] !== undefined) {
      fail(`${extensionId}: scripts.${retired} is retired and read by nothing — remove it`);
    }
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
