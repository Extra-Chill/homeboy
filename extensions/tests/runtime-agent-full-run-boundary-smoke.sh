#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

REPO_ROOT="$repo_root" node <<'NODE'
const fs = require('node:fs');
const path = require('node:path');

const root = process.env.REPO_ROOT;
const forbidden = /datamachine|data machine|wpsg|wp_gym|wp-gym/i;
const files = [
  '.github/workflows/runtime-agent-full-run.yml',
  '.github/scripts/runtime-agent-full-run/auth.cjs',
  '.github/scripts/runtime-agent-full-run/materialize-dependencies.cjs',
  '.github/scripts/runtime-agent-full-run/setup-runtime.cjs',
  '.github/scripts/runtime-agent-full-run/project-engine-data.cjs',
  '.github/scripts/runtime-agent-full-run/assert-success.cjs',
  '.github/scripts/runtime-agent-full-run/artifacts-and-comments.cjs',
  '.github/scripts/runtime-agent-full-run/build-runner-config.cjs',
  '.github/scripts/runtime-agent-full-run/lib/common.cjs',
  'wordpress/scripts/agent/run-runtime-agent-task.cjs',
];

const failures = [];
const compatibilityReferences = [];
for (const file of files) {
  const body = fs.readFileSync(path.join(root, file), 'utf8');
  const match = body.match(forbidden);
  if (match) {
    failures.push(`${file}: ${match[0]}`);
  }

  if (body.includes('domain-specific-agent-ci')) {
    compatibilityReferences.push(file);
  }
}

if (failures.length > 0) {
  process.stderr.write(`Generic runtime full-run boundary violations:\n${failures.join('\n')}\n`);
  process.exit(1);
}

if (compatibilityReferences.length > 0) {
  process.stderr.write(`Generic runtime full-run references compatibility wrapper files:\n${compatibilityReferences.join('\n')}\n`);
  process.exit(1);
}
NODE
