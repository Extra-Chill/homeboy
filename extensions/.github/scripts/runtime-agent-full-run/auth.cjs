#!/usr/bin/env node
'use strict';

const { normalizeContextRepositories, requireRepo, splitCsv, writeGithubOutput } = require('./lib/common.cjs');
const {
  secretEnvMapSourceNames,
} = require('../../../runtime-agent-ci/lib/secret-env-plan.cjs');
const fs = require('node:fs');
const { randomUUID } = require('node:crypto');

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
  if (command === 'materialize-secret-env') {
    materializeSecretEnv(process.env);
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

function materializeSecretEnv(env) {
  const configPath = requireValue(env.CONFIG_FILE, 'CONFIG_FILE');
  const githubEnvPath = requireValue(env.GITHUB_ENV, 'GITHUB_ENV');
  const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  const secretEnvMap = config.secret_env_map;
  if (!secretEnvMap || typeof secretEnvMap !== 'object' || Array.isArray(secretEnvMap) || Object.keys(secretEnvMap).length === 0) {
    throw new Error('materialize_secret_env_from_github_secrets requires a non-empty secret_env_map declaration.');
  }

  const githubSecrets = parseGithubSecretsJson(env.HOMEBOY_GITHUB_SECRETS_JSON || '{}');
  const sourceNames = secretEnvMapSourceNames(secretEnvMap);
  if (sourceNames.length === 0) {
    throw new Error('secret_env_map must declare at least one source env name.');
  }

  const materializedNames = [];
  const lines = [];
  for (const sourceName of sourceNames) {
    const value = githubSecrets[sourceName];
    if (typeof value !== 'string' || value === '') {
      continue;
    }
    const delimiter = `HOMEBOY_SECRET_${randomUUID().replace(/-/g, '_')}`;
    materializedNames.push(sourceName);
    lines.push(`${sourceName}<<${delimiter}`, value, delimiter);
  }
  if (lines.length > 0) {
    fs.appendFileSync(githubEnvPath, `${lines.join('\n')}\n`);
  }
  process.stdout.write(`Materialized ${materializedNames.length} declared GitHub secret env source(s): ${materializedNames.join(', ') || 'none'}\n`);
}

function parseGithubSecretsJson(raw) {
  const parsed = JSON.parse(raw);
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('HOMEBOY_GITHUB_SECRETS_JSON must be a JSON object.');
  }
  return parsed;
}

function requireValue(value, name) {
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`${name} is required.`);
  }
  return value;
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}
