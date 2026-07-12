#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';

const rootDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');

const extensionIds = new Set([
  'discord',
  'go',
  'managed-preview',
  'nodejs',
  'rust',
  'swift',
  'wordpress',
]);

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

  for (const [scriptName, scriptPath] of Object.entries(manifest.scripts || {})) {
    if (typeof scriptPath === 'string') {
      assertRelativeFile(extensionId, scriptPath, `scripts.${scriptName}`);
    }
  }

  walk(manifest, (key, value, keyPath) => {
    if (key === 'extension_script' && typeof value === 'string') {
      assertRelativeFile(extensionId, value, keyPath.join('.'));
    }

    if ((key === 'setup_command' || key === 'run_command' || key === 'command') && typeof value === 'string') {
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
