'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  calculatePublishSet,
  evaluateSideEffectPolicy,
  publicationTemplates,
  publishWorkspace,
  validateWritablePaths,
} = require('../lib/workspace-publication-lifecycle.cjs');

const writable = validateWritablePaths({ writable_paths: ['docs/**', 'README.md'] }, [
  './docs/generated.md',
  'README.md',
  'plugins/amp/AGENTS.md',
]);
assert.equal(writable.enabled, true);
assert.equal(writable.success, false);
assert.deepEqual(writable.patterns, ['docs/**', 'README.md']);
assert.deepEqual(writable.rejected_files, ['plugins/amp/AGENTS.md']);
assert.match(writable.error, /Changed files outside writable_paths: plugins\/amp\/AGENTS\.md/);

const sideEffects = evaluateSideEffectPolicy({ declared_side_effect_paths: ['build/**'] }, [
  'build/output.txt',
  'tmp/cache.txt',
]);
assert.equal(sideEffects.enabled, true);
assert.equal(sideEffects.success, false);
assert.deepEqual(sideEffects.accepted_files, ['build/output.txt']);
assert.deepEqual(sideEffects.rejected_files, ['tmp/cache.txt']);
assert.match(sideEffects.error, /Verification side-effect files outside declared policy: tmp\/cache\.txt/);

assert.deepEqual(
  calculatePublishSet(['docs/generated.md', './README.md'], {
    accepted_files: ['build/output.txt', 'docs/generated.md'],
  }),
  ['docs/generated.md', 'README.md', 'build/output.txt']
);

const templates = publicationTemplates({
  runner_workspace: {
    branch: 'agent-artifacts/{agent_slug}-{run_id}',
    from: 'origin/trunk',
  },
  artifact_export: {
    commit_message_template: 'chore: publish {task_id}',
    pr_title_template: 'Publish {result_label}',
    pr_body_template: 'Provider: {provider}\nModel: {model_label}\n',
  },
}, {
  agent_slug: 'fixture-agent',
  run_id: '12345',
  task_id: 'fixture-workload',
  result_label: 'workspace changes',
  provider: 'openai',
  model_label: 'gpt-5.5',
}, { env: { GITHUB_BASE_REF: 'ignored-by-runner-workspace-from' } });
assert.deepEqual(templates, {
  branch: 'agent-artifacts/fixture-agent-12345',
  commitMessage: 'chore: publish fixture-workload',
  title: 'Publish workspace changes',
  body: 'Provider: openai\nModel: gpt-5.5\n',
  base: 'trunk',
});

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-workspace-publication-lifecycle.'));
try {
  const workspace = path.join(tmp, 'repo');
  fs.mkdirSync(path.join(workspace, 'docs'), { recursive: true });
  fs.writeFileSync(path.join(workspace, 'docs', 'generated.md'), '# Generated\n');

  const calls = [];
  const run = (command, args, options = {}) => {
    calls.push({ command, args, cwd: options.cwd });
    if (command === 'git' && args[0] === 'status') {
      return { status: 0, stdout: '?? docs/generated.md\n', stderr: '' };
    }
    if (command === 'git' && args[0] === 'diff' && args[1] === '--cached') {
      return { status: 0, stdout: 'docs/generated.md\n', stderr: '' };
    }
    if (command === 'gh' && args[0] === 'pr' && args[1] === 'view') {
      return { status: 1, stdout: '', stderr: 'not found' };
    }
    if (command === 'gh' && args[0] === 'pr' && args[1] === 'create') {
      return { status: 0, stdout: 'https://github.com/owner/repo/pull/1291\n', stderr: '' };
    }
    return { status: 0, stdout: '', stderr: '' };
  };

  const publication = publishWorkspace({
    target_repo: 'owner/repo',
    agent_slug: 'fixture-agent',
    provider: 'openai',
    model: 'gpt-5.5',
    workload_id: 'fixture-workload',
    runner_workspace: { branch: 'agent-artifacts/{agent_slug}-{run_id}', from: 'origin/trunk' },
    artifact_export: {
      commit_message_template: 'chore: persist {task_id}',
      pr_title_template: 'Persist {result_label}',
      pr_body_template: 'Provider: {provider}\nModel: {model_label}\n',
    },
  }, {}, {
    id: 'fixture-workload',
    metadata: { job_id: 'job-1' },
  }, workspace, ['docs/generated.md'], {
    env: { GITHUB_RUN_ID: '12345' },
    run,
    tmpdir: tmp,
  });

  assert.equal(publication.opened, true);
  assert.equal(publication.success, true);
  assert.equal(publication.repo, 'owner/repo');
  assert.equal(publication.head, 'agent-artifacts/fixture-agent-12345');
  assert.equal(publication.base, 'trunk');
  assert.equal(publication.url, 'https://github.com/owner/repo/pull/1291');
  assert.equal(publication.action, 'created');
  assert.deepEqual(publication.files, ['docs/generated.md']);
  assert.deepEqual(calls.filter((call) => call.command === 'gh').map((call) => call.args.slice(0, 2)), [
    ['pr', 'view'],
    ['pr', 'create'],
  ]);
  assert.equal(calls.some((call) => call.command === 'git' && call.args[0] === 'push'), true);
  assert.equal(calls.some((call) => call.command === 'git' && call.args[0] === 'commit'), true);
} finally {
  fs.rmSync(tmp, { recursive: true, force: true });
}

process.stdout.write('Workspace publication lifecycle primitive checks passed\n');
