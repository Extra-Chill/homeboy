'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
  calculatePublishSet,
  ensureReviewRequest,
  evaluateSideEffectPolicy,
  preparePublication,
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

const prepared = preparePublication({
  target_repo: 'owner/repo',
  agent_slug: 'fixture agent',
  provider: 'openai',
  model: 'gpt-5.5',
  workload_id: 'fixture-workload',
  runner_workspace: { branch: 'agent artifacts/{agent_slug}-{run_id}', from: 'origin/trunk' },
}, {}, {
  id: 'fixture-workload',
  metadata: { job_id: 'job-1' },
}, ['docs/generated.md'], {
  env: { GITHUB_RUN_ID: '12345', HOMEBOY_HOST_LIFECYCLE_DRY_RUN: '1' },
});
assert.equal(prepared.changed, true);
assert.equal(prepared.dry_run, true);
assert.equal(prepared.branch, 'agent-artifacts/fixture-agent-12345');
assert.deepEqual(prepared.publication_evidence_ref, {
  type: 'branch',
  provider: 'github',
  repo: 'owner/repo',
  head: 'agent-artifacts/fixture-agent-12345',
  base: 'trunk',
  url: '',
  action: '',
  pr_number: null,
  pr_state: '',
  files: ['docs/generated.md'],
});

{
  const calls = [];
  const review = ensureReviewRequest('/workspace', 'agent-artifacts/fixture-agent-12345', {
    base: 'trunk',
    title: 'Updated title',
    body: 'Updated body',
  }, {
    run(command, args, options = {}) {
      calls.push({ command, args, cwd: options.cwd });
      if (command === 'gh' && args[0] === 'pr' && args[1] === 'view') {
        return { status: 0, stdout: JSON.stringify({ number: 47, state: 'OPEN', url: 'https://github.com/owner/repo/pull/47' }), stderr: '' };
      }
      return { status: 0, stdout: '', stderr: '' };
    },
  });
  assert.equal(review.action, 'updated');
  assert.equal(review.url, 'https://github.com/owner/repo/pull/47');
  assert.deepEqual(review.publication_evidence_ref, {
    type: 'pull_request',
    provider: 'github',
    repo: '',
    head: 'agent-artifacts/fixture-agent-12345',
    base: 'trunk',
    url: 'https://github.com/owner/repo/pull/47',
    action: 'updated',
    pr_number: 47,
    pr_state: 'OPEN',
    files: [],
  });
  assert.deepEqual(calls.filter((call) => call.command === 'gh').map((call) => call.args.slice(0, 2)), [
    ['pr', 'view'],
    ['pr', 'edit'],
  ]);
}

{
  const calls = [];
  const review = ensureReviewRequest('/workspace', 'agent-artifacts/fixture-agent-12345', {
    base: 'trunk',
    title: 'Updated title',
    body: 'Updated body',
  }, {
    env: { GITHUB_RUN_ID: '999' },
    run(command, args, options = {}) {
      calls.push({ command, args, cwd: options.cwd });
      if (command === 'gh' && args[0] === 'pr' && args[1] === 'view') {
        return { status: 0, stdout: JSON.stringify({ number: 47, state: 'CLOSED', url: 'https://github.com/owner/repo/pull/47' }), stderr: '' };
      }
      if (command === 'gh' && args[0] === 'pr' && args[1] === 'reopen') {
        return { status: 1, stdout: '', stderr: 'Could not open the pull request' };
      }
      if (command === 'gh' && args[0] === 'pr' && args[1] === 'create') {
        return { status: 0, stdout: 'https://github.com/owner/repo/pull/1291\n', stderr: '' };
      }
      return { status: 0, stdout: '', stderr: '' };
    },
  });
  assert.equal(review.action, 'created_after_closed_pr');
  assert.equal(review.head, 'agent-artifacts/fixture-agent-12345-run-999');
  assert.equal(review.closed_pr_number, 47);
  assert.match(review.reopen_error, /Could not open the pull request/);
  assert.equal(review.publication_evidence_ref.type, 'pull_request');
  assert.equal(review.publication_evidence_ref.head, 'agent-artifacts/fixture-agent-12345-run-999');
  assert.equal(review.publication_evidence_ref.action, 'created_after_closed_pr');
  assert.deepEqual(calls.filter((call) => call.command === 'gh').map((call) => call.args.slice(0, 2)), [
    ['pr', 'view'],
    ['pr', 'reopen'],
    ['pr', 'create'],
  ]);
  assert.equal(calls.some((call) => call.command === 'git' && call.args[0] === 'push'), true);
}

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
    if (command === 'homeboy' && args[0] === 'agent-task' && args[1] === 'finalize-pr') {
      return {
        status: 0,
        stdout: `${JSON.stringify({
          schema: 'homeboy/agent-task-pr-finalization/v1',
          run_id: '12345',
          status: 'review_ready',
          path: workspace,
          base: 'trunk',
          head: 'agent-artifacts/fixture-agent-12345',
          pr_action: 'created',
          pr_number: 1291,
          pr_url: 'https://github.com/owner/repo/pull/1291',
          changed_files: ['docs/generated.md'],
          publication_intent: {
            schema: 'homeboy/agent-task-publication-intent/v1',
            run_id: '12345',
            action: 'review_request',
            target: { kind: 'code_review', adapter: 'github_pull_request', base: 'trunk', head: 'agent-artifacts/fixture-agent-12345' },
            changed_files: ['docs/generated.md'],
          },
          publication_proof: {
            schema: 'homeboy/agent-task-publication-proof/v1',
            run_id: '12345',
            status: 'review_ready',
            intent_schema: 'homeboy/agent-task-publication-intent/v1',
            target: { kind: 'code_review', adapter: 'github_pull_request', base: 'trunk', head: 'agent-artifacts/fixture-agent-12345', url: 'https://github.com/owner/repo/pull/1291' },
            adapter_action: 'created',
            adapter_ref: 'https://github.com/owner/repo/pull/1291',
          },
        })}\n`,
        stderr: '',
      };
    }
    return { status: 0, stdout: '', stderr: '' };
  };

  const publication = publishWorkspace({
    target_repo: 'owner/repo',
    agent_slug: 'fixture-agent',
    provider: 'openai',
    model: 'gpt-5.5',
    workload_id: 'fixture-workload',
    finalization_gate_results: [{ id: 'verification_commands', status: 'passed' }],
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
  assert.equal(publication.pr_number, 1291);
  assert.deepEqual(publication.files, ['docs/generated.md']);
  assert.equal(publication.finalization.schema, 'homeboy/agent-task-pr-finalization/v1');
  assert.equal(publication.publication_intent.schema, 'homeboy/agent-task-publication-intent/v1');
  assert.equal(publication.publication_proof.schema, 'homeboy/agent-task-publication-proof/v1');
  assert.deepEqual(publication.publication_evidence_ref, {
    type: 'pull_request',
    provider: 'github',
    repo: 'owner/repo',
    head: 'agent-artifacts/fixture-agent-12345',
    base: 'trunk',
    url: 'https://github.com/owner/repo/pull/1291',
    action: 'created',
    pr_number: 1291,
    pr_state: 'OPEN',
    files: ['docs/generated.md'],
  });
  const finalizationCall = calls.find((call) => call.command === 'homeboy' && call.args[0] === 'agent-task' && call.args[1] === 'finalize-pr');
  assert.ok(finalizationCall, 'publication delegates review finalization to Homeboy');
  assert.equal(finalizationCall.args.includes('--gate-result'), true);
  assert.equal(finalizationCall.args.includes('verification_commands=passed'), true);
  assert.equal(finalizationCall.args.includes('--changed-file'), true);
  assert.equal(finalizationCall.args.includes('docs/generated.md'), true);
  assert.equal(calls.some((call) => call.command === 'gh'), false);
  assert.equal(calls.some((call) => call.command === 'git' && call.args[0] === 'push'), false);
  assert.equal(calls.some((call) => call.command === 'git' && call.args[0] === 'commit'), false);
} finally {
  fs.rmSync(tmp, { recursive: true, force: true });
}

process.stdout.write('Workspace publication lifecycle primitive checks passed\n');
