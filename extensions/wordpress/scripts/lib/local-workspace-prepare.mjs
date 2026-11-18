/**
 * Apply declared `local_workspace_dependencies` before host-side
 * `wp_codebox_prepare_steps` run (homeboy#18302).
 *
 * Prepare steps such as `npm run build` resolve the component's `file:` links
 * to sibling workspace packages. Without the override those links point at
 * unbuilt sources (no `dist/`), so the build fails. The shared shell bridge
 * builds, packs, and installs each declared dependency into the consumer; it is
 * a no-op when nothing is declared and cheap when the tarball is already
 * installed.
 */
import { existsSync } from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const collectionDirectory = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');

export function declaresLocalWorkspaceDependencies(settings) {
  const entries = settings?.local_workspace_dependencies;
  return entries !== undefined && !(Array.isArray(entries) && entries.length === 0);
}

export function applyLocalWorkspaceDependenciesBeforePrepare({ settings, steps, consumerDirectory, env = process.env }) {
  if (!Array.isArray(steps) || steps.length === 0 || !declaresLocalWorkspaceDependencies(settings)) {
    return false;
  }
  if (!existsSync(path.join(consumerDirectory, 'package.json'))) {
    return false;
  }
  const bridge = path.join(collectionDirectory, 'scripts', 'lib', 'local-workspace-install.sh');
  const result = spawnSync('bash', [
    '-c',
    'source "$1"; homeboy_apply_local_workspace_install "$2" "$3"',
    '_',
    bridge,
    consumerDirectory,
    collectionDirectory,
  ], {
    cwd: consumerDirectory,
    env: { ...env, HOMEBOY_SETTINGS_JSON: JSON.stringify(settings) },
    encoding: 'utf8',
    // Keep the adapter's stdout clean; override progress is diagnostics.
    stdio: ['ignore', 2, 2],
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`local_workspace_dependencies override failed before wp_codebox_prepare_steps (exit ${result.status ?? 1})`);
  }
  return true;
}
