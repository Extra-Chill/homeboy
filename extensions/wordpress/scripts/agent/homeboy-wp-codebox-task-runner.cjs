#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function argValues(name) {
  const values = [];
  for (let index = 0; index < process.argv.length; index += 1) {
    if (process.argv[index] === name && process.argv[index + 1]) {
      values.push(process.argv[index + 1]);
    }
  }
  return values;
}

function hasFlag(name) {
  return process.argv.includes(name);
}

function usage() {
  console.error('Usage: homeboy-wp-codebox-task-runner.cjs --agents-api <path> --data-machine <path> --data-machine-code <path> [--wp-codebox-bin <bin>] [--provider <id>] [--model <id>] [--provider-plugin-path <path>] [--secret-env <ENV>] [--mount <host:vfs[:mode]>] [--artifacts <dir>]');
  process.exit(1);
}

function readStdin() {
  try {
    return fs.readFileSync(0, 'utf8');
  } catch {
    return '';
  }
}

function readTaskRequest() {
  const raw = process.env.HOMEBOY_WP_CODEBOX_TASK_REQUEST || readStdin();
  if (!raw.trim()) {
    throw new Error('Task request JSON is required on stdin or HOMEBOY_WP_CODEBOX_TASK_REQUEST.');
  }
  const request = JSON.parse(raw);
  if (!request || request.schema !== 'homeboy/wp-codebox-task-request/v1') {
    throw new Error('Task request must use schema homeboy/wp-codebox-task-request/v1.');
  }
  return request;
}

function requireArg(name) {
  const value = argValue(name);
  if (!value) {
    usage();
  }
  return value;
}

function pluginEntry(source, slug, activate = false) {
  return { source, slug: slug || path.basename(source), activate };
}

function providerPluginEntries(request) {
  const paths = [...(request.provider_plugin_paths || []), ...argValues('--provider-plugin-path')];
  return paths.map((source) => pluginEntry(source, path.basename(source), false));
}

function secretEnvNames(request) {
  return Array.from(new Set([...(request.secret_env || []), ...argValues('--secret-env')].filter(Boolean)));
}

function mountEntries() {
  return argValues('--mount').map((value) => {
    const [source, target, mode = 'readwrite'] = value.split(':');
    if (!source || !target) {
      throw new Error(`Invalid --mount value: ${value}`);
    }
    return {
      source,
      target,
      mode,
      metadata: { kind: 'homeboy-audit-fanout' },
    };
  });
}

function recipeForRequest(request, options) {
  const provider = argValue('--provider') || request.provider || '';
  const model = argValue('--model') || request.model || '';
  const workspaceSlug = path.basename(options.agentsApi).split('@')[0].replace(/[^a-zA-Z0-9_-]/g, '-');
  const task = JSON.stringify({
    schema: 'homeboy/wp-codebox-audit-task/v1',
    sandbox_session_id: request.sandbox_session_id,
    group_key: request.group_key,
    orchestrator: request.orchestrator,
    audit_findings: request.audit_findings,
    task: {
      ...(request.task || {}),
      prompt: [
        request.task?.prompt || '',
        `Use Data Machine Code workspace repo \`${workspaceSlug}\` for all workspace_* tool calls.`,
      ].filter(Boolean).join('\n\n'),
    },
  });

  const providerSlugs = providerPluginEntries(request).map((plugin) => plugin.slug).join(',');
  const stepArgs = [
    `task=${task}`,
    'agent=sandbox-agent',
    'mode=sandbox',
    `provider=${provider}`,
    `model=${model}`,
    `provider-plugin-slugs=${providerSlugs}`,
  ];

  return {
    schema: 'wp-codebox/workspace-recipe/v1',
    runtime: {
      wp: argValue('--wp') || 'latest',
      blueprint: { steps: [] },
    },
    inputs: {
      workspaces: [
        {
          seed: {
            type: 'directory',
            source: options.agentsApi,
            slug: workspaceSlug,
          },
          target: `/workspace/${workspaceSlug}`,
          mode: 'readwrite',
          sourceMode: 'repo-backed',
        },
      ],
      mounts: mountEntries(),
      extraPlugins: [
        pluginEntry(options.agentsApi, 'agents-api', false),
        pluginEntry(options.dataMachine, 'data-machine', false),
        pluginEntry(options.dataMachineCode, 'data-machine-code', false),
        ...providerPluginEntries(request),
      ],
      secretEnv: secretEnvNames(request),
    },
    workflow: {
      steps: [
        {
          command: 'wp-codebox.agent-sandbox-run',
          args: stepArgs,
        },
      ],
    },
  };
}

function writeRecipe(recipe) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-recipe-'));
  const recipePath = path.join(directory, 'recipe.json');
  fs.writeFileSync(recipePath, `${JSON.stringify(recipe, null, 2)}\n`);
  return recipePath;
}

function runWpCodebox(recipePath) {
  const wpCodeboxBin = argValue('--wp-codebox-bin') || 'wp-codebox';
  const args = ['recipe-run', '--recipe', recipePath, '--json'];
  const artifacts = argValue('--artifacts');
  if (artifacts) {
    args.push('--artifacts', artifacts);
  }
  if (argValue('--preview-hold')) {
    args.push('--preview-hold', argValue('--preview-hold'));
  }
  if (argValue('--preview-public-url')) {
    args.push('--preview-public-url', argValue('--preview-public-url'));
  }

  if (hasFlag('--print-command')) {
    console.error(JSON.stringify({ command: wpCodeboxBin, args }, null, 2));
  }

  const result = spawnSync(wpCodeboxBin, args, {
    encoding: 'utf8',
    env: process.env,
    maxBuffer: 1024 * 1024 * 20,
  });

  if (result.stdout) {
    process.stdout.write(result.stdout);
  }
  if (result.stderr) {
    process.stderr.write(result.stderr);
  }
  return result.status ?? 1;
}

try {
  const request = readTaskRequest();
  const options = {
    agentsApi: requireArg('--agents-api'),
    dataMachine: requireArg('--data-machine'),
    dataMachineCode: requireArg('--data-machine-code'),
  };
  const recipe = recipeForRequest(request, options);
  const recipePath = writeRecipe(recipe);

  if (hasFlag('--print-recipe')) {
    console.error(JSON.stringify({ recipePath, recipe }, null, 2));
  }

  process.exitCode = runWpCodebox(recipePath);
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exitCode = 1;
}
