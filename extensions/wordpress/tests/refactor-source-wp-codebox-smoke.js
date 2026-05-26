'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-refactor-source-wp-codebox-'));

try {
  const auditResult = JSON.parse(fs.readFileSync(
    path.join(__dirname, 'fixtures', 'homeboy-audit-wp-codebox-fanout', 'audit-report.json'),
    'utf8'
  ));
  const outputDir = path.join(root, 'fanout');
  const command = {
    command: 'refactor_source',
    source: 'audit',
    component_id: 'fixture-plugin',
    root: '/repo/fixture-plugin',
    source_result: auditResult,
    write: false,
    settings: {
      wp_codebox_output_dir: outputDir,
      wp_codebox_provider: 'opencode',
      wp_codebox_model: 'opencode-go/kimi-k2.6',
      wp_codebox_provider_plugin_paths: '/plugins/ai-provider-for-opencode',
      wp_codebox_secret_env: 'OPENCODE_API_KEY',
      wp_codebox_issue_url: 'https://github.com/Extra-Chill/homeboy-extensions/issues/769',
    },
  };

  const result = spawnSync('python3', [path.join(__dirname, '..', 'scripts', 'refactor.py')], {
    encoding: 'utf8',
    input: JSON.stringify(command),
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const response = JSON.parse(result.stdout);
  assert.equal(response.handled, true);
  assert.equal(response.detected_findings, 3);
  assert.equal(response.changed_files.length, 0);
  assert.equal(response.fix_results.length, 2);
  assert.equal(response.fix_results[0].rule, 'wp_codebox.audit_fanout');
  assert.equal(response.fix_results[0].primitive, 'extension_refactor_source');
  assert.match(response.warnings[0], /fanout-plan\.json/);

  const plan = JSON.parse(fs.readFileSync(path.join(outputDir, 'fanout-plan.json'), 'utf8'));
  assert.equal(plan.schema, 'homeboy/audit-wp-codebox-fanout/v1');
  assert.equal(plan.task_requests.length, 2);
  assert.equal(plan.task_requests[0].provider, 'opencode');
  assert.equal(plan.task_requests[0].model, 'opencode-go/kimi-k2.6');
  assert.deepEqual(plan.task_requests[0].provider_plugin_paths, ['/plugins/ai-provider-for-opencode']);
  assert.deepEqual(plan.task_requests[0].secret_env, ['OPENCODE_API_KEY']);

  console.log('WordPress refactor source WP Codebox smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
