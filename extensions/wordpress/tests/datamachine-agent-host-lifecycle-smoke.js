'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = path.join(__dirname, '..');
const script = path.join(root, 'scripts', 'agent', 'run-host-runner-lifecycle.cjs');

function run(command, args, options = {}) {
  return spawnSync(command, args, {
    cwd: options.cwd,
    env: { ...process.env, ...(options.env || {}) },
    encoding: 'utf8',
  });
}

function checked(command, args, options = {}) {
  const result = run(command, args, options);
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return result;
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function makeRepo(tmp) {
  const repo = path.join(tmp, 'repo');
  const origin = path.join(tmp, 'origin.git');
  fs.mkdirSync(repo, { recursive: true });
  checked('git', ['init', '-b', 'trunk'], { cwd: repo });
  checked('git', ['config', 'user.name', 'Fixture User'], { cwd: repo });
  checked('git', ['config', 'user.email', 'fixture@example.test'], { cwd: repo });
  fs.writeFileSync(path.join(repo, 'README.md'), '# Fixture\n');
  checked('git', ['add', 'README.md'], { cwd: repo });
  checked('git', ['commit', '-m', 'Initial commit'], { cwd: repo });
  checked('git', ['init', '--bare', origin]);
  checked('git', ['remote', 'add', 'origin', origin], { cwd: repo });
  checked('git', ['push', '-u', 'origin', 'trunk'], { cwd: repo });
  return repo;
}

function makeGhFixture(tmp, options = {}) {
  const bin = path.join(tmp, 'bin');
  const log = path.join(tmp, 'gh.log');
  fs.mkdirSync(bin, { recursive: true });
  const gh = path.join(bin, 'gh');
  fs.writeFileSync(gh, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
fs.appendFileSync(${JSON.stringify(log)}, process.argv.slice(2).join(' ') + '\\n');
const existing = ${JSON.stringify(options.existingPullRequest || null)};
if (process.argv[2] === 'pr' && process.argv[3] === 'view') {
  if (!existing) process.exit(1);
  process.stdout.write(JSON.stringify(existing) + '\\n');
  process.exit(0);
}
if (process.argv[2] === 'pr' && process.argv[3] === 'create') {
  process.stdout.write('https://github.com/owner/repo/pull/1291\\n');
  process.exit(0);
}
if (process.argv[2] === 'pr' && process.argv[3] === 'reopen') {
  if (${JSON.stringify(Boolean(options.failReopen))}) {
    process.stderr.write('API call failed: GraphQL: Could not open the pull request. (reopenPullRequest)');
    process.exit(1);
  }
  process.exit(0);
}
if (process.argv[2] === 'pr' && process.argv[3] === 'edit') process.exit(0);
process.stderr.write('unexpected gh call: ' + process.argv.slice(2).join(' '));
process.exit(1);
`);
  fs.chmodSync(gh, 0o755);
  return { bin, log };
}

function makeRunFiles(tmp, config) {
  const configPath = path.join(tmp, 'config.json');
  const resultsPath = path.join(tmp, 'results.json');
  writeJson(configPath, {
    target_repo: 'owner/repo',
    agent_slug: 'docs-agent',
    provider: 'openai',
    model: 'gpt-5.5',
    workload_id: 'developer-docs',
    runner_workspace: { branch: 'agent-artifacts/docs-agent-host-lifecycle', from: 'origin/trunk' },
    artifact_export: {
      commit_message_template: 'chore: persist generated docs',
      pr_title_template: 'Persist generated docs',
      pr_body_template: '## Result\nGenerated docs are ready.\n',
    },
    ...config,
  });
  writeJson(resultsPath, {
    component_id: 'datamachine-agent-ci-driver',
    scenarios: [{ id: 'developer-docs', metrics: {}, metadata: { engine_data: {}, ...(config.initial_metadata || {}) } }],
  });
  return { configPath, resultsPath };
}

{
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-host-lifecycle-success.'));
  const repo = makeRepo(tmp);
  const gh = makeGhFixture(tmp);
  checked('git', ['checkout', '-B', 'agent-artifacts/docs-agent-host-lifecycle'], { cwd: repo });
  fs.writeFileSync(path.join(repo, 'stale.txt'), 'old branch content\n');
  checked('git', ['add', 'stale.txt'], { cwd: repo });
  checked('git', ['commit', '-m', 'Stale generated docs'], { cwd: repo });
  checked('git', ['push', '-u', 'origin', 'agent-artifacts/docs-agent-host-lifecycle'], { cwd: repo });
  checked('git', ['checkout', 'trunk'], { cwd: repo });
  fs.writeFileSync(path.join(repo, 'generated.txt'), 'hello from agent\n');
  const { configPath, resultsPath } = makeRunFiles(tmp, {
    writable_paths: ['generated.txt'],
    verification_commands: [{ command: 'test -f generated.txt', description: 'Generated file exists' }],
    drift_checks: [{ command: 'git diff --exit-code', description: 'Verification did not create unstaged drift' }],
  });

  const result = run('node', [script, '--results', resultsPath, '--config', configPath, '--scenario', 'developer-docs', '--workspace', repo], {
    env: { PATH: `${gh.bin}${path.delimiter}${process.env.PATH}`, GITHUB_RUN_ID: '12345', GH_TOKEN: 'fixture' },
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = readJson(resultsPath);
  const scenario = output.scenarios[0];
  assert.equal(scenario.metrics.verification_commands_succeeded, 1);
  assert.equal(scenario.metrics.drift_checks_succeeded, 1);
  assert.equal(scenario.metrics.writable_paths_satisfied, 1);
  assert.equal(scenario.metrics.pr_opened, 1);
  assert.equal(scenario.metadata.success_status, 'pr_opened');
  assert.equal(scenario.metadata.runner_workspace_publication.url, 'https://github.com/owner/repo/pull/1291');
  assert.match(fs.readFileSync(gh.log, 'utf8'), /pr create .*--base trunk/);
  checked('git', ['fetch', 'origin', 'agent-artifacts/docs-agent-host-lifecycle'], { cwd: repo });
  const publishedFiles = checked(
    'git',
    ['ls-tree', '-r', '--name-only', 'origin/agent-artifacts/docs-agent-host-lifecycle'],
    { cwd: repo },
  ).stdout;
  assert.match(publishedFiles, /generated\.txt/);
  assert.doesNotMatch(publishedFiles, /stale\.txt/);
}

{
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-host-lifecycle-writable-paths.'));
  const repo = makeRepo(tmp);
  const gh = makeGhFixture(tmp);
  fs.mkdirSync(path.join(repo, 'plugins', 'amp'), { recursive: true });
  fs.writeFileSync(path.join(repo, 'plugins', 'amp', 'AGENTS.md'), 'invalid docs lane output\n');
  const { configPath, resultsPath } = makeRunFiles(tmp, {
    writable_paths: ['README.md', 'docs/**', 'plugins/**/README.md'],
    verification_commands: [{ command: 'test -f plugins/amp/AGENTS.md', description: 'Out-of-policy file exists' }],
  });

  const result = run('node', [script, '--results', resultsPath, '--config', configPath, '--scenario', 'developer-docs', '--workspace', repo], {
    env: { PATH: `${gh.bin}${path.delimiter}${process.env.PATH}`, GITHUB_RUN_ID: '12345', GH_TOKEN: 'fixture' },
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Changed files outside writable_paths: plugins\/amp\/AGENTS\.md/);
  const output = readJson(resultsPath);
  const scenario = output.scenarios[0];
  assert.equal(output.status, 'failed');
  assert.equal(scenario.metrics.writable_paths_satisfied, 0);
  assert.equal(scenario.metrics.pr_opened, 0);
  assert.deepEqual(scenario.metadata.runner_writable_path_policy.rejected_files, ['plugins/amp/AGENTS.md']);
  assert.equal(fs.existsSync(gh.log), false);
}

{
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-host-lifecycle-closed-pr.'));
  const repo = makeRepo(tmp);
  const gh = makeGhFixture(tmp, {
    existingPullRequest: { number: 47, state: 'CLOSED', url: 'https://github.com/owner/repo/pull/47' },
  });
  checked('git', ['checkout', '-B', 'agent-artifacts/docs-agent-host-lifecycle'], { cwd: repo });
  fs.writeFileSync(path.join(repo, 'stale.txt'), 'old branch content\n');
  checked('git', ['add', 'stale.txt'], { cwd: repo });
  checked('git', ['commit', '-m', 'Stale generated docs'], { cwd: repo });
  checked('git', ['push', '-u', 'origin', 'agent-artifacts/docs-agent-host-lifecycle'], { cwd: repo });
  fs.writeFileSync(path.join(repo, 'generated.txt'), 'hello from agent on stale branch\n');
  const { configPath, resultsPath } = makeRunFiles(tmp, {
    initial_metadata: { success_status: 'write_without_pr', completion_outcome_satisfied: false },
    verification_commands: [{ command: 'test -f generated.txt', description: 'Generated file exists' }],
  });

  const result = run('node', [script, '--results', resultsPath, '--config', configPath, '--scenario', 'developer-docs', '--workspace', repo], {
    env: { PATH: `${gh.bin}${path.delimiter}${process.env.PATH}`, GITHUB_RUN_ID: '12345', GH_TOKEN: 'fixture' },
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = readJson(resultsPath);
  assert.equal(output.scenarios[0].metadata.success_status, 'pr_opened');
  assert.equal(output.scenarios[0].metadata.completion_outcome_satisfied, false);
  const publication = output.scenarios[0].metadata.runner_workspace_publication;
  assert.equal(publication.url, 'https://github.com/owner/repo/pull/47');
  assert.equal(publication.action, 'reopened');
  assert.equal(publication.pr_state, 'OPEN');
  const ghLog = fs.readFileSync(gh.log, 'utf8');
  assert.match(ghLog, /pr reopen 47/);
  assert.doesNotMatch(ghLog, /pr create/);
  checked('git', ['fetch', 'origin', 'agent-artifacts/docs-agent-host-lifecycle'], { cwd: repo });
  const publishedFiles = checked(
    'git',
    ['ls-tree', '-r', '--name-only', 'origin/agent-artifacts/docs-agent-host-lifecycle'],
    { cwd: repo },
  ).stdout;
  assert.match(publishedFiles, /generated\.txt/);
  assert.doesNotMatch(publishedFiles, /stale\.txt/);
}

{
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-host-lifecycle-closed-pr-replacement.'));
  const repo = makeRepo(tmp);
  const gh = makeGhFixture(tmp, {
    existingPullRequest: { number: 47, state: 'CLOSED', url: 'https://github.com/owner/repo/pull/47' },
    failReopen: true,
  });
  checked('git', ['checkout', '-B', 'agent-artifacts/docs-agent-host-lifecycle'], { cwd: repo });
  fs.writeFileSync(path.join(repo, 'stale.txt'), 'old branch content\n');
  checked('git', ['add', 'stale.txt'], { cwd: repo });
  checked('git', ['commit', '-m', 'Stale generated docs'], { cwd: repo });
  checked('git', ['push', '-u', 'origin', 'agent-artifacts/docs-agent-host-lifecycle'], { cwd: repo });
  fs.writeFileSync(path.join(repo, 'generated.txt'), 'hello from replacement branch\n');
  const { configPath, resultsPath } = makeRunFiles(tmp, {
    verification_commands: [{ command: 'test -f generated.txt', description: 'Generated file exists' }],
  });

  const result = run('node', [script, '--results', resultsPath, '--config', configPath, '--scenario', 'developer-docs', '--workspace', repo], {
    env: { PATH: `${gh.bin}${path.delimiter}${process.env.PATH}`, GITHUB_RUN_ID: '12345', GH_TOKEN: 'fixture' },
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = readJson(resultsPath);
  const publication = output.scenarios[0].metadata.runner_workspace_publication;
  assert.equal(publication.url, 'https://github.com/owner/repo/pull/1291');
  assert.equal(publication.action, 'created_after_closed_pr');
  assert.equal(publication.head, 'agent-artifacts/docs-agent-host-lifecycle-run-12345');
  assert.equal(publication.closed_pr_number, 47);
  assert.match(publication.reopen_error, /Could not open the pull request/);
  const ghLog = fs.readFileSync(gh.log, 'utf8');
  assert.match(ghLog, /pr reopen 47/);
  assert.match(ghLog, /pr create --head agent-artifacts\/docs-agent-host-lifecycle-run-12345 --base trunk/);
  checked('git', ['fetch', 'origin', 'agent-artifacts/docs-agent-host-lifecycle-run-12345'], { cwd: repo });
  const publishedFiles = checked(
    'git',
    ['ls-tree', '-r', '--name-only', 'origin/agent-artifacts/docs-agent-host-lifecycle-run-12345'],
    { cwd: repo },
  ).stdout;
  assert.match(publishedFiles, /generated\.txt/);
  assert.doesNotMatch(publishedFiles, /stale\.txt/);
}

{
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-host-lifecycle-drift-fail.'));
  const repo = makeRepo(tmp);
  const gh = makeGhFixture(tmp);
  fs.writeFileSync(path.join(repo, 'generated.txt'), 'hello from agent\n');
  const { configPath, resultsPath } = makeRunFiles(tmp, {
    verification_commands: [{
      command: 'printf "\\nverifier drift\\n" >> README.md',
      description: 'Verifier mutates tracked output',
    }],
    drift_checks: [{ command: 'git diff --exit-code', description: 'Verifier output must be committed' }],
  });

  const result = run('node', [script, '--results', resultsPath, '--config', configPath, '--scenario', 'developer-docs', '--workspace', repo], {
    env: { PATH: `${gh.bin}${path.delimiter}${process.env.PATH}`, GITHUB_RUN_ID: '12345', GH_TOKEN: 'fixture' },
  });
  assert.notEqual(result.status, 0);
  const output = readJson(resultsPath);
  const drift = output.scenarios[0].metadata.drift_check_results.checks[0];
  assert.equal(output.status, 'failed');
  assert.equal(output.scenarios[0].metrics.verification_commands_succeeded, 1);
  assert.equal(output.scenarios[0].metrics.drift_checks_succeeded, 0);
  assert.equal(drift.command, 'git diff --exit-code');
  assert.match(drift.stdout, /README\.md/);
  assert.equal(fs.existsSync(gh.log), false);
}

{
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-host-lifecycle-fail.'));
  const repo = makeRepo(tmp);
  const gh = makeGhFixture(tmp);
  fs.writeFileSync(path.join(repo, 'generated.txt'), 'hello from agent\n');
  const { configPath, resultsPath } = makeRunFiles(tmp, {
    verification_commands: [{ command: 'printf useful-output && grep -q missing generated.txt', description: 'Generated file has missing content' }],
    drift_checks: [{ command: 'printf should-not-run', description: 'Skipped drift check' }],
  });

  const result = run('node', [script, '--results', resultsPath, '--config', configPath, '--scenario', 'developer-docs', '--workspace', repo], {
    env: { PATH: `${gh.bin}${path.delimiter}${process.env.PATH}`, GITHUB_RUN_ID: '12345', GH_TOKEN: 'fixture' },
  });
  assert.notEqual(result.status, 0);
  const output = readJson(resultsPath);
  const check = output.scenarios[0].metadata.verification_results.checks[0];
  assert.equal(output.status, 'failed');
  assert.equal(output.scenarios[0].metrics.verification_commands_succeeded, 0);
  assert.equal(output.scenarios[0].metadata.drift_check_results.skipped_reason, 'verification_commands_failed');
  assert.equal(check.command, 'printf useful-output && grep -q missing generated.txt');
  assert.equal(check.prepared_command, 'printf useful-output && grep -q missing generated.txt');
  assert.equal(check.exit_code, 1);
  assert.equal(check.stdout, 'useful-output');
  assert.equal(fs.existsSync(gh.log), false);
}

console.log('Data Machine agent host lifecycle smoke passed');
