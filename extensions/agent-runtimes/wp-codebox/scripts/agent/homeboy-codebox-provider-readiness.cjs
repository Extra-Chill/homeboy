#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const {
  wpCodeboxRuntimeReadinessDiagnostics,
} = require('../../lib/wp-codebox-runtime-readiness');

const REQUEST_SCHEMA = 'homeboy/agent-task-provider-readiness-request/v1';
const RESULT_SCHEMA = 'homeboy/agent-task-provider-readiness-result/v1';

function main() {
  const request = JSON.parse(fs.readFileSync(0, 'utf8'));
  if (request?.schema !== REQUEST_SCHEMA || !request.effective_config || typeof request.effective_config !== 'object') {
    throw new Error(`WP Codebox provider readiness requires ${REQUEST_SCHEMA} with effective_config.`);
  }

  const config = request.effective_config;
  const diagnostics = wpCodeboxRuntimeReadinessDiagnostics({
    runtime_overlays: config.runtime_overlays || config.wp_codebox_runtime_overlays || [],
    runtime_overlay_profiles: config.runtime_overlay_profiles || [],
  }, {
    wpCodeboxCoreModule: config.wp_codebox_core_module || config.wpCodeboxCoreModule,
  });
  process.stdout.write(JSON.stringify({
    schema: RESULT_SCHEMA,
    ready: diagnostics.length === 0,
    message: diagnostics[0]?.message || 'WP Codebox provider runtime is ready.',
    diagnostics,
  }));
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error.stack || error.message}\n`);
  process.exitCode = 1;
}
