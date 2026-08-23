import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { preflightWpCodeboxCommand } = require(path.join(root, 'agent-runtimes/wp-codebox/lib/wp-codebox-runtime-selection.js'));
const readiness = path.join(root, 'agent-runtimes/wp-codebox/scripts/agent/homeboy-wp-codebox-runner-readiness.cjs');
const manifest = JSON.parse(fs.readFileSync(path.join(root, 'agent-runtimes/wp-codebox/wp-codebox.json'), 'utf8'));
const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'hbe-wp-codebox-runner-readiness-'));
const run = (env) => {
  const result = spawnSync(process.execPath, [readiness], { encoding: 'utf8', env: { ...process.env, ...env } });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
};
const executable = (file, version, browserPreview = true, invocationMarker = '') => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `#!/usr/bin/env node
${invocationMarker ? `require('node:fs').writeFileSync(${JSON.stringify(invocationMarker)}, 'invoked');` : ''}
if (process.argv.includes('--version')) process.stdout.write(${JSON.stringify(version)});
else if (process.argv.slice(-3).join(' ') === 'runtime descriptor --json') process.stdout.write(JSON.stringify(${JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: browserPreview ? 'ready' : 'unavailable' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: browserPreview ? 'wp-codebox/browser-contained-site-open/v1' : '' } } } })}));
else process.exit(1);
`);
  fs.chmodSync(file, 0o755);
};
const managedRuntime = (root, version, browserPreview = true) => {
  const source = path.join(root, 'source');
  const cli = path.join(source, 'packages/cli/dist/index.js');
  executable(cli, version, browserPreview);
  spawnSync('git', ['init', '-q'], { cwd: source });
  spawnSync('git', ['add', '.'], { cwd: source });
  spawnSync('git', ['-c', 'user.name=Test', '-c', 'user.email=test@example.test', 'commit', '-qm', 'fixture'], { cwd: source });
  const sha = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: source, encoding: 'utf8' }).stdout.trim();
  fs.writeFileSync(path.join(source, '.homeboy-runtime-identity.json'), JSON.stringify({ schema: 'homeboy/wp-codebox-managed-runtime-identity/v1', source_sha: sha, cli_sha256: createHash('sha256').update(fs.readFileSync(cli)).digest('hex'), required_capabilities: ['wp-codebox/browser-contained-site-open/v1'] }));
  return cli;
};

try {
  const declaration = manifest.agent_task_executors[0].runner_readiness[0];
  assert.deepEqual(declaration.invocation.argv, ['node', '{{runtime_path}}/scripts/agent/homeboy-wp-codebox-runner-readiness.cjs']);
  assert.equal(declaration.remediation, 'homeboy extension setup wordpress');
  assert.equal(manifest.minimum_version, '0.21.0');
  assert.equal(manifest.version, '1.5.4');
   const stalePath = path.join(temp, 'stale-path');
   const stale = path.join(stalePath, 'wp-codebox');
   const pathInvocation = path.join(temp, 'path-preflight');
   executable(stale, '0.12.27', true, pathInvocation);
  const incomplete = path.join(temp, 'incomplete');
  fs.mkdirSync(path.join(incomplete, 'source/packages/cli'), { recursive: true });

  const dangling = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete, HOMEBOY_WP_CODEBOX_BIN: path.join(temp, 'missing'), PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(dangling.ready, false);
  assert.equal(dangling.classification, 'wp_codebox_configured_binary_missing');
  assert.equal(dangling.remediation, 'homeboy extension setup wordpress');
  assert.equal(dangling.identity.executable, path.join(temp, 'missing'));
  assert.equal(dangling.identity.source, 'configured');
  assert.equal(dangling.identity.version, '');
  assert.equal(dangling.candidates.configured.path, path.join(temp, 'missing'));
  assert.equal(dangling.candidates.managed.path, path.join(incomplete, 'source/packages/cli/dist/index.js'));
  assert.equal(dangling.candidates.path.path, stale);

  const incompleteResult = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete, HOMEBOY_WP_CODEBOX_BIN: '', PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(incompleteResult.ready, false);
  assert.equal(incompleteResult.classification, 'wp_codebox_managed_binary_missing');
  assert.equal(incompleteResult.identity.executable, path.join(incomplete, 'source/packages/cli/dist/index.js'));
  assert.equal(incompleteResult.identity.source, 'managed');

  const healthy = path.join(temp, 'healthy');
  const managedCli = path.join(healthy, 'source/packages/cli/dist/index.js');
  managedRuntime(healthy, '0.19.0');
  const managed = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: healthy, HOMEBOY_WP_CODEBOX_BIN: '', PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(managed.ready, false);
  assert.equal(managed.classification, 'wp_codebox_version_too_old');
  assert.equal(managed.identity.executable, managedCli);
  assert.equal(managed.identity.source, 'managed');
  assert.equal(managed.identity.version, '0.19.0');

  const cachedCurrent = path.join(temp, 'cached-current');
  const cachedCli = managedRuntime(cachedCurrent, '0.21.0');
   const cached = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: cachedCurrent, PATH: `${stalePath}:${process.env.PATH}` });
   assert.equal(cached.ready, true);
  assert.equal(cached.identity.executable, cachedCli);
  assert.equal(cached.identity.source, 'managed');
   assert.equal(cached.identity.version, '0.21.0');

   const packagedRoot = path.join(temp, 'packaged-runtime');
   const packagedComponent = path.join(packagedRoot, 'plugin');
   const packagedCli = path.join(packagedRoot, 'cli/dist/index.js');
   fs.mkdirSync(packagedComponent, { recursive: true });
   executable(packagedCli, '0.21.0');
   const packaged = run({
     HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete,
     HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT: packagedComponent,
     PATH: `${stalePath}:${process.env.PATH}`,
   });
   assert.equal(packaged.ready, true);
   assert.equal(packaged.identity.executable, packagedCli);
   assert.equal(packaged.identity.source, 'packaged');
   assert.equal(packaged.candidates.path.path, stale);
   assert.equal(packaged.candidates.path.available, true);
   assert.equal(fs.existsSync(pathInvocation), false, 'packaged selection must not preflight an unrelated PATH runtime');
   const alternatePackaged = run({
     HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete,
     WP_CODEBOX_RUNTIME_COMPONENT: packagedComponent,
     PATH: `${stalePath}:${process.env.PATH}`,
   });
   assert.equal(alternatePackaged.ready, true);
   assert.equal(alternatePackaged.identity.executable, packagedCli);
   assert.equal(alternatePackaged.identity.source, 'packaged');

  const staleBuild = path.join(temp, 'stale-build');
  managedRuntime(staleBuild, '0.21.0');
  fs.writeFileSync(path.join(staleBuild, 'source', '.homeboy-runtime-identity.json'), JSON.stringify({ schema: 'homeboy/wp-codebox-managed-runtime-identity/v1', source_sha: '0'.repeat(40), cli_sha256: '0'.repeat(64), required_capabilities: ['wp-codebox/browser-contained-site-open/v1'] }));
  assert.equal(run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: staleBuild, PATH: `${stalePath}:${process.env.PATH}` }).classification, 'wp_codebox_managed_source_identity_invalid');

   const tamperedBuild = path.join(temp, 'tampered-build');
   const tamperedCli = managedRuntime(tamperedBuild, '0.21.0');
   fs.appendFileSync(tamperedCli, '\n// tampered after setup\n');
   assert.equal(run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: tamperedBuild, PATH: `${stalePath}:${process.env.PATH}` }).classification, 'wp_codebox_managed_source_identity_invalid');
   // A caller may pass the managed path explicitly. That preserves managed
   // provenance rather than converting the path into an unverified override.
   const tamperedConfigured = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: tamperedBuild, HOMEBOY_WP_CODEBOX_BIN: tamperedCli, PATH: `${stalePath}:${process.env.PATH}` });
   assert.equal(tamperedConfigured.classification, 'wp_codebox_managed_source_identity_invalid');
   assert.equal(tamperedConfigured.identity.source, 'managed');
   const tamperedCommand = preflightWpCodeboxCommand([process.execPath, tamperedCli], { env: { ...process.env, HOMEBOY_WP_CODEBOX_INSTALL_DIR: tamperedBuild } });
    assert.equal(tamperedCommand.reason, 'wp_codebox_managed_source_identity_invalid');
    assert.equal(tamperedCommand.selected.source, 'managed');

    const tamperedSymlink = path.join(temp, 'tampered-managed-symlink');
    fs.symlinkSync(tamperedCli, tamperedSymlink);
    const tamperedSymlinked = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: tamperedBuild, HOMEBOY_WP_CODEBOX_BIN: tamperedSymlink, PATH: `${stalePath}:${process.env.PATH}` });
    assert.equal(tamperedSymlinked.classification, 'wp_codebox_managed_source_identity_invalid');
    assert.equal(tamperedSymlinked.identity.source, 'managed');

  const missingCapability = path.join(temp, 'missing-capability');
  managedRuntime(missingCapability, '0.21.0', false);
  assert.equal(run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: missingCapability, PATH: `${stalePath}:${process.env.PATH}` }).classification, 'wp_codebox_browser_preview_capability_missing');

  const override = path.join(temp, 'override');
  executable(override, '0.21.0');
  const explicit = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete, HOMEBOY_WP_CODEBOX_BIN: override, PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(explicit.ready, true);
  assert.equal(explicit.identity.executable, override);
  assert.equal(explicit.identity.source, 'configured');
  assert.equal(explicit.identity.version, '0.21.0');
  assert.equal(explicit.required_version, '0.21.0');

  const settingsBin = path.join(temp, 'settings-bin');
  executable(settingsBin, '0.21.0');
  const settingsOnly = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete, HOMEBOY_SETTINGS_WP_CODEBOX_BIN: settingsBin, PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(settingsOnly.ready, true);
  assert.equal(settingsOnly.identity.executable, settingsBin);
  assert.equal(settingsOnly.identity.source, 'configured');

  const jsonSettingsBin = path.join(temp, 'json-settings-bin');
  executable(jsonSettingsBin, '0.21.0');
  const jsonSettingsOnly = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete, HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: jsonSettingsBin }), PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(jsonSettingsOnly.ready, true);
  assert.equal(jsonSettingsOnly.identity.executable, jsonSettingsBin);
  assert.equal(jsonSettingsOnly.identity.source, 'configured');

  const currentManaged = path.join(temp, 'current-managed');
  executable(path.join(currentManaged, 'source/packages/cli/dist/index.js'), '0.21.0');
  const oldOverride = path.join(temp, 'old-override');
  executable(oldOverride, '0.19.0');
  const configuredWins = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: currentManaged, HOMEBOY_WP_CODEBOX_BIN: oldOverride, PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(configuredWins.ready, false);
  assert.equal(configuredWins.identity.executable, oldOverride);
  assert.equal(configuredWins.identity.source, 'configured');

  const prerelease = path.join(temp, 'prerelease');
  executable(prerelease, '0.21.0-rc.1');
  const prereleaseResult = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete, HOMEBOY_WP_CODEBOX_BIN: prerelease, PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(prereleaseResult.ready, false);
  assert.equal(prereleaseResult.classification, 'wp_codebox_version_too_old');
  assert.equal(prereleaseResult.identity.version, '0.21.0-rc.1');
} finally {
  fs.rmSync(temp, { recursive: true, force: true });
}

console.log('wp-codebox runner readiness smoke passed');
