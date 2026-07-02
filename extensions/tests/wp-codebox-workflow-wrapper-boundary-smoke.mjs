#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const wrapper = fs.readFileSync(path.join(repoRoot, '.github/workflows/wp-codebox-runtime-agent-full-run.yml'), 'utf8');
const genericWorkflow = fs.readFileSync(path.join(repoRoot, '.github/workflows/runtime-agent-full-run.yml'), 'utf8');
const genericDocs = fs.readFileSync(path.join(repoRoot, '.github/workflows/README.md'), 'utf8');
const wpDocs = fs.readFileSync(path.join(repoRoot, 'docs/wp-codebox-runtime-workflow.md'), 'utf8');
const manifest = JSON.parse(fs.readFileSync(path.join(repoRoot, 'agent-runtimes/wp-codebox/wp-codebox.json'), 'utf8'));

assert.match(wrapper, /runtime: wp-codebox/);
assert.match(wrapper, /runtime_wordpress_version: \$\{\{ inputs\.wordpress_version \}\}/);
assert.match(wrapper, /extra_wp_config_defines: \$\{\{ inputs\.wp_config_defines \}\}/);
assert.match(wrapper, /runtime_mounts: \$\{\{ inputs\.wp_runtime_mounts \}\}/);
assert.match(wrapper, /runtime_overlays: \$\{\{ inputs\.wp_runtime_overlays \}\}/);
assert.match(wrapper, /wordpress_version:[\s\S]*?default: ''/);
assert.match(wrapper, /wp_config_defines:[\s\S]*?default: ''/);
assert.match(wrapper, /wp_runtime_mounts:[\s\S]*?default: ''/);
assert.match(wrapper, /wp_runtime_overlays:[\s\S]*?default: ''/);

assert.match(genericWorkflow, /runtime_wordpress_version:[\s\S]*?default: ''/);
assert.match(genericWorkflow, /extra_wp_config_defines:[\s\S]*?default: ''/);
assert.doesNotMatch(genericDocs, /runtime_wordpress_version: beta/);
assert.doesNotMatch(genericDocs, /extra_wp_config_defines:/);
assert.doesNotMatch(genericDocs, /wp-content\/plugins\/example-runtime-plugin/);
assert.doesNotMatch(genericDocs, /wp-codebox-runtime-agent-full-run\.yml/);

assert.match(wpDocs, /wp-codebox-runtime-agent-full-run\.yml/);
assert.match(wpDocs, /wordpress_version: beta/);
assert.match(wpDocs, /wp_config_defines:/);
assert.match(wpDocs, /wp_runtime_mounts:/);
assert.equal(manifest.workflow_input_projection.wrapper.workflow, '.github/workflows/wp-codebox-runtime-agent-full-run.yml');
assert.deepEqual(manifest.workflow_input_projection.wrapper.input_defaults, {
  wordpress_version: '7.0',
  wp_config_defines: {},
  wp_runtime_mounts: [],
  wp_runtime_overlays: [],
});
assert.deepEqual(manifest.workflow_input_projection.wrapper.input_mapping, {
  wordpress_version: 'runtime_wordpress_version',
  wp_config_defines: 'extra_wp_config_defines',
  wp_runtime_mounts: 'runtime_mounts',
  wp_runtime_overlays: 'runtime_overlays',
});

console.log('wp-codebox workflow wrapper boundary smoke passed');
