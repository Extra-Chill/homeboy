'use strict';

const fs = require('node:fs');
const path = require('node:path');

function setupRuntime({ phase, workspace, env = process.env, run, installCheckedOutPhpDependencies }) {
  // After the runtime's build commands run, the wp-codebox CLI exists in the
  // checkout (packages/cli/dist/index.js, made executable by the build) but is
  // not on PATH and HOMEBOY_WP_CODEBOX_BIN is unset, so the downstream
  // "Build runner config" step reports the bin as missing. Point the env var at
  // the built CLI so subsequent steps resolve runtime.paths.runtime_bin. Runs
  // for the wp-codebox runtime regardless of the WordPress-dependency gate.
  if (phase === 'after_commands') {
    exportRuntimeBin(workspace, env);
  }
  if (!requiresWordPressDependencies(env)) {
    return;
  }
  if (phase === 'before_commands') {
    run('composer', ['install', '--no-interaction', '--no-progress', '--prefer-dist'], { cwd: path.join(workspace, '.ci/homeboy-extensions/wordpress') });
    run('npm', ['install'], { cwd: path.join(workspace, '.ci/homeboy-extensions/wordpress') });
    return;
  }
  if (phase === 'after_commands') {
    installCheckedOutPhpDependencies(workspace);
  }
}

function exportRuntimeBin(workspace, env = process.env) {
  // Already provided by the caller/env — don't override an explicit bin.
  if (env.HOMEBOY_WP_CODEBOX_BIN || env.WP_CODEBOX_BIN) {
    return;
  }
  const binPath = path.join(workspace, '.ci/wp-codebox/packages/cli/dist/index.js');
  if (!fs.existsSync(binPath)) {
    // Build did not produce the CLI; let the downstream check surface a clear,
    // accurate "Runtime CLI build missing" error rather than a wrong path.
    return;
  }
  const githubEnv = env.GITHUB_ENV;
  if (githubEnv) {
    fs.appendFileSync(githubEnv, `HOMEBOY_WP_CODEBOX_BIN=${binPath}\n`);
  }
}

function requiresWordPressDependencies(env = process.env) {
  const runtimeProfileId = env.PROFILE || env.RUNTIME_PROFILE || '';
  if (!runtimeProfileId) {
    return true;
  }
  let profiles;
  try {
    profiles = env.RUNTIME_PROFILES ? JSON.parse(env.RUNTIME_PROFILES) : {};
  } catch {
    return true;
  }
  const profile = profiles && typeof profiles === 'object' && !Array.isArray(profiles) ? profiles[runtimeProfileId] : null;
  if (!profile || typeof profile !== 'object' || Array.isArray(profile)) {
    return true;
  }
  return profile.requires_wordpress_dependencies !== false && profile.wordpress_dependencies !== false;
}

module.exports = {
  requiresWordPressDependencies,
  setupRuntime,
};
