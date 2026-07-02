#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const wrapper = fs.readFileSync(path.join(repoRoot, '.github/workflows/wp-codebox-runtime-agent-full-run.yml'), 'utf8');
const genericDocs = fs.readFileSync(path.join(repoRoot, '.github/workflows/README.md'), 'utf8');
const wpDocs = fs.readFileSync(path.join(repoRoot, 'docs/wp-codebox-runtime-workflow.md'), 'utf8');

assert.match(wrapper, /runtime: wp-codebox/);
assert.match(wrapper, /runtime_wordpress_version: \$\{\{ inputs\.wordpress_version \}\}/);
assert.match(wrapper, /extra_wp_config_defines: \$\{\{ inputs\.wp_config_defines \}\}/);
assert.match(wrapper, /runtime_mounts: \$\{\{ inputs\.wp_runtime_mounts \}\}/);
assert.match(wrapper, /runtime_overlays: \$\{\{ inputs\.wp_runtime_overlays \}\}/);

assert.doesNotMatch(genericDocs, /runtime_wordpress_version: beta/);
assert.doesNotMatch(genericDocs, /extra_wp_config_defines:/);
assert.doesNotMatch(genericDocs, /wp-content\/plugins\/example-runtime-plugin/);
assert.match(genericDocs, /wp-codebox-runtime-agent-full-run\.yml/);

assert.match(wpDocs, /wp-codebox-runtime-agent-full-run\.yml/);
assert.match(wpDocs, /wordpress_version: beta/);
assert.match(wpDocs, /wp_config_defines:/);
assert.match(wpDocs, /wp_runtime_mounts:/);

console.log('wp-codebox workflow wrapper boundary smoke passed');
