#!/usr/bin/env node
'use strict';

const { spawnSync } = require('node:child_process');
const path = require('node:path');

const RESULT_SCHEMA = 'homeboy/agent-task-provider-readiness-result/v1';
const REPAIR_COMMAND = 'homeboy extension setup wordpress';
const paths = path.resolve(__dirname, '../../../../wordpress/scripts/lib/wp-codebox-paths.sh');

function resolve() {
  const result = spawnSync('bash', ['-c', [
    'source "$1"',
    'override="${HOMEBOY_WP_CODEBOX_BIN:-${WP_CODEBOX_BIN:-${HOMEBOY_SETTINGS_WP_CODEBOX_BIN:-}}}"',
    'if [ -n "$override" ]; then',
    '  if homeboy_wp_codebox_bin_is_present "$override"; then printf "explicit_override\\t%s\\n" "$override"; exit 0; fi',
    '  printf "configured_binary_missing\\t%s\\n" "$override"; exit 2',
    'fi',
    'if homeboy_wp_codebox_managed_cache_is_incomplete; then',
    '  printf "managed_cache_incomplete\\t%s\\n" "$(homeboy_wp_codebox_managed_cli_candidates | head -1)"; exit 2',
    'fi',
    'bin="$(homeboy_wp_codebox_resolve_bin "${HOMEBOY_SETTINGS_JSON:-}")" || exit 1',
    'printf "resolved\\t%s\\n" "$bin"',
  ].join('\n'), 'wp-codebox-runner-readiness', paths], {
    encoding: 'utf8',
    env: process.env,
  });
  const [source = '', executable = ''] = result.stdout.trim().split('\t');
  return { status: result.status, source, executable, stderr: result.stderr.trim() };
}

function version(executable) {
  const command = /\.(?:c|m)?js$/.test(executable) ? process.execPath : executable;
  const args = command === executable ? ['--version'] : [executable, '--version'];
  const result = spawnSync(command, args, { encoding: 'utf8', env: process.env });
  return result.status === 0 ? (result.stdout.trim() || result.stderr.trim()) : '';
}

function main() {
  const resolved = resolve();
  if (resolved.status !== 0 || !resolved.executable) {
    const reason = resolved.source === 'configured_binary_missing'
      ? `Configured WP Codebox binary is missing: ${resolved.executable}.`
      : resolved.source === 'managed_cache_incomplete'
        ? `Managed WP Codebox cache is incomplete; built CLI entrypoint is missing: ${resolved.executable}.`
        : resolved.stderr || 'WP Codebox executable could not be resolved.';
    process.stdout.write(JSON.stringify({
      schema: RESULT_SCHEMA,
      ready: false,
      classification: resolved.source || 'wp_codebox_unavailable',
      retryable: false,
      remediation: REPAIR_COMMAND,
      reason,
      cache_key: '',
      identity: { executable: resolved.executable, source: resolved.source },
    }));
    return;
  }
  const selectedVersion = version(resolved.executable);
  if (!selectedVersion) {
    process.stdout.write(JSON.stringify({
      schema: RESULT_SCHEMA,
      ready: false,
      classification: 'wp_codebox_version_probe_failed',
      retryable: false,
      remediation: REPAIR_COMMAND,
      reason: `Resolved WP Codebox executable did not answer --version: ${resolved.executable}.`,
      cache_key: '',
      identity: { executable: resolved.executable, source: resolved.source },
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
    identity: { executable: resolved.executable, source: resolved.source, version: selectedVersion },
  }));
}

main();
