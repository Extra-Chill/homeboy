#!/usr/bin/env node
'use strict';

const { normalizeContextRepositories, requireRepo, splitCsv, writeGithubOutput } = require('./lib/common.cjs');

function main() {
  const command = process.argv[2];
  if (command === 'detect-credentials') {
    writeGithubOutput({ available: process.env.HOMEBOY_APP_ID && process.env.HOMEBOY_APP_PRIVATE_KEY ? 'true' : 'false' });
    return;
  }
  if (command === 'resolve-token-scope') {
    const scope = resolveTokenScope(process.env);
    writeGithubOutput(scope);
    return;
  }
  if (command === 'report-mode') {
    reportMode(process.env);
    return;
  }
  throw new Error(`Unknown auth command: ${command || ''}`);
}

function resolveTokenScope(env) {
  requireRepo(env.TARGET_REPO, 'target_repo');
  const contextRepos = normalizeContextRepositories(env.CONTEXT_REPOSITORIES || '[]').map((entry) => entry.repo);
  const entries = splitCsv(env.APP_TOKEN_REPOS || env.TARGET_REPO).concat(env.APP_TOKEN_REPOS ? [] : contextRepos);
  let owner = '';
  const repositories = [];
  for (const entry of entries) {
    requireRepo(entry, 'app_token_repos entries');
    const [entryOwner, repoName] = entry.split('/');
    if (!owner) {
      owner = entryOwner;
    } else if (owner !== entryOwner) {
      throw new Error(`actions/create-github-app-token supports one owner per token; got ${owner} and ${entryOwner}`);
    }
    repositories.push(repoName);
  }
  if (!owner) {
    throw new Error('No repositories resolved for app token.');
  }
  return { owner, repositories: repositories.join(',') };
}

function reportMode(env) {
  const hasAppToken = Boolean(env.APP_TOKEN);
  const authMode = hasAppToken ? 'homeboy_app_token' : 'github_token_fallback';
  const tokenScope = hasAppToken ? `${env.APP_TOKEN_OWNER}/${env.APP_TOKEN_REPOSITORIES}` : env.TARGET_REPO;
  const tokenNote = hasAppToken ? 'Generated Homeboy GitHub App installation token.' : 'Using repository-scoped github.token fallback.';
  const contextCount = normalizeContextRepositories(env.CONTEXT_REPOSITORIES || '[]').length;

  writeGithubOutput({ auth_mode: authMode });
  process.stdout.write(`Runtime agent full-run auth_mode: ${authMode}\n`);
  process.stdout.write(`Token scope: ${tokenScope}\n`);
  process.stdout.write(`require_homeboy_app_token: ${env.REQUIRE_HOMEBOY_APP_TOKEN}\n`);
  process.stdout.write(`dry_run: ${env.DRY_RUN}\n`);

  if (env.GITHUB_STEP_SUMMARY) {
    require('node:fs').appendFileSync(
      env.GITHUB_STEP_SUMMARY,
      [
        '## Runtime Agent Full-Run Auth',
        '',
        `- Auth mode: \`${authMode}\``,
        `- Target repository: \`${env.TARGET_REPO}\``,
        `- Token scope: \`${tokenScope}\``,
        `- Homeboy App token repos input: \`${env.APP_TOKEN_REPOS_INPUT || 'default target_repo'}\``,
        `- Context repositories: \`${contextCount}\``,
        `- Require Homeboy App token: \`${env.REQUIRE_HOMEBOY_APP_TOKEN}\``,
        `- Note: ${tokenNote}`,
        '',
      ].join('\n')
    );
  }
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}
