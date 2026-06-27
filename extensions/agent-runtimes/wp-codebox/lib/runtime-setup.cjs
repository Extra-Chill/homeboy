'use strict';

const path = require('node:path');

function setupRuntime({ phase, workspace, env = process.env, run, installCheckedOutPhpDependencies }) {
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
