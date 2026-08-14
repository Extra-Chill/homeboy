#!/usr/bin/env node
'use strict';

const { preflightWpCodeboxRuntime } = require('../../lib/wp-codebox-runtime-selection');

const RESULT_SCHEMA = 'homeboy/agent-task-provider-readiness-result/v1';
function main() {
  const result = preflightWpCodeboxRuntime();
  if (!result.ready) {
    process.stdout.write(JSON.stringify({
      schema: RESULT_SCHEMA,
      ready: false,
      classification: result.reason,
      retryable: false,
      remediation: result.remediation,
      reason: `WP Codebox runtime preflight failed: ${result.reason}.`,
      cache_key: '',
      identity: { executable: result.selected.path, source: result.selected.source, version: result.selected.version },
      candidates: result.candidates,
      required_version: result.required_version,
    }));
    return;
  }
  process.stdout.write(JSON.stringify({
    schema: RESULT_SCHEMA,
    ready: true,
    classification: 'ready',
    retryable: false,
    remediation: '',
    reason: 'WP Codebox runner is ready.',
    cache_key: '',
    identity: { executable: result.selected.path, source: result.selected.source, version: result.selected.version },
    candidates: result.candidates,
    required_version: result.required_version,
  }));
}

main();
