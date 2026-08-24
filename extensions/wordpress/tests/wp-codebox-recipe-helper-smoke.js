'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {
  homeboySettings,
  parseWpCodeboxJson,
  recipeEventName,
  runWpCodeboxRecipe,
  wpCodeboxBin,
  wpCodeboxCommand,
  wpCodeboxPluginStateStep,
} = require('../lib/wp-codebox-recipe-helper');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-recipe-helper-'));
const fixtureBin = path.join(root, 'fixture-wp-codebox.cjs');
const capturePath = path.join(root, 'capture.json');
const recipeFile = path.join(root, 'recipe.json');
const artifactsDir = path.join(root, 'artifacts');
const outputFile = path.join(root, 'output', 'wp-codebox-output.json');
const managedInstall = path.join(root, 'managed');
const managedSource = path.join(managedInstall, 'source');
const managedBin = path.join(managedSource, 'packages', 'cli', 'dist', 'index.js');

fs.writeFileSync(recipeFile, JSON.stringify({ schema: 'wp-codebox/workspace-recipe/v1' }));
fs.writeFileSync(fixtureBin, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args.slice(-3).join(' ') === 'runtime descriptor --json') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }
fs.writeFileSync(process.env.FIXTURE_CAPTURE_PATH, JSON.stringify({ argv: process.argv.slice(2) }, null, 2));
if (process.env.FIXTURE_FAIL) {
  process.stdout.write(JSON.stringify({ ok: false, failure: true }));
  process.exit(7);
}
if (process.env.FIXTURE_LARGE_STDOUT) {
  process.stdout.write(JSON.stringify({ ok: true, payload: 'x'.repeat(4096) }));
  process.exit(0);
}
if (process.env.FIXTURE_HANG) {
  // Simulate a wedged recipe-run: spawn a sub-process (to prove group-kill) and
  // then hang forever. Optionally ignore SIGTERM to force the SIGKILL escalation.
  const { spawn } = require('node:child_process');
  const grandchild = spawn(process.execPath, ['-e', 'process.on(\\'SIGTERM\\', () => {}); setInterval(() => {}, 1e9);']);
  fs.writeFileSync(process.env.FIXTURE_PIDS_PATH, JSON.stringify({ child: process.pid, grandchild: grandchild.pid }));
  if (process.env.FIXTURE_IGNORE_SIGTERM) {
    process.on('SIGTERM', () => {});
  }
  setInterval(() => {}, 1e9);
  return;
}
process.stdout.write(JSON.stringify({ ok: true, artifacts: { directory: process.argv[process.argv.indexOf('--artifacts') + 1] } }));
`);
fs.chmodSync(fixtureBin, 0o755);

function pidAlive(pid) {
  if (typeof pid !== 'number') {
    return false;
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === 'EPERM';
  }
}

async function waitForReaped(pids, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!pids.some(pidAlive)) {
      return true;
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return false;
}

(async () => {
  assert.equal(wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_BIN: fixtureBin } }), fixtureBin);
  assert.equal(wpCodeboxBin({ env: { HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: fixtureBin }) } }), fixtureBin);
  assert.equal(wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_BIN: '/env/wp-codebox', HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: fixtureBin }) } }), '/env/wp-codebox');
  assert.deepEqual(homeboySettings({ HOMEBOY_SETTINGS_JSON: '{"wp_codebox_bin":"/bin/wp-codebox"}' }), { wp_codebox_bin: '/bin/wp-codebox' });
  assert.deepEqual(homeboySettings({ HOMEBOY_SETTINGS_JSON: 'not json' }), {});
  // Canonical selection may supply an installed runtime when no explicit pin is
  // present; execution still performs the exact-command preflight below.
  assert.equal(typeof wpCodeboxBin({ env: {} }), 'string');
  assert.deepEqual(wpCodeboxCommand('/tmp/wp-codebox.cjs'), { command: process.execPath, args: ['/tmp/wp-codebox.cjs'] });
  assert.deepEqual(wpCodeboxCommand('wp-codebox'), { command: 'wp-codebox', args: [] });
  assert.deepEqual(
    wpCodeboxPluginStateStep({ activate: ['source-plugin/source-plugin.php'], deactivate: [{ slug: 'old-plugin' }] }),
    {
      command: 'wordpress.plugin-state',
      args: ['plugin-state-json={"activate":[{"plugin":"source-plugin/source-plugin.php"}],"deactivate":[{"slug":"old-plugin","plugin":"old-plugin"}],"report":true}'],
    }
  );
  assert.equal(recipeEventName('start'), 'recipe.start');
  assert.equal(recipeEventName('start', { eventPrefix: 'sandbox.recipe' }), 'sandbox.recipe.start');
  assert.deepEqual(parseWpCodeboxJson(' {"ok": true}\n'), { ok: true });

  const events = [];
  const result = await runWpCodeboxRecipe({
    recipeFile,
    artifactsDir,
    outputFile,
    wpCodeboxBin: fixtureBin,
    recipeRunArgs: ['--fixture'],
    env: { ...process.env, FIXTURE_CAPTURE_PATH: capturePath },
    event: (source, name, data) => events.push({ source, name, data }),
  });

  assert.deepEqual(result.json, { ok: true, artifacts: { directory: artifactsDir } });
  assert.deepEqual(JSON.parse(fs.readFileSync(outputFile, 'utf8')), result.json);
  assert.deepEqual(events.map((entry) => [entry.source, entry.name]), [
    ['wp_codebox', 'recipe.start'],
    ['wp_codebox', 'recipe.done'],
  ]);
  const capture = JSON.parse(fs.readFileSync(capturePath, 'utf8'));
  assert.deepEqual(capture.argv, ['recipe-run', '--recipe', recipeFile, '--artifacts', artifactsDir, '--fixture', '--json']);

  // Managed selection is provenance-checked before execution. This fixture is
  // a real immutable-style source checkout with a matching identity record.
  fs.mkdirSync(path.dirname(managedBin), { recursive: true });
  fs.copyFileSync(fixtureBin, managedBin);
  fs.chmodSync(managedBin, 0o755);
  require('node:child_process').spawnSync('git', ['init', '-q'], { cwd: managedSource });
  require('node:child_process').spawnSync('git', ['add', '.'], { cwd: managedSource });
  require('node:child_process').spawnSync('git', ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test', 'commit', '-qm', 'fixture'], { cwd: managedSource });
  const managedSha = require('node:child_process').spawnSync('git', ['rev-parse', 'HEAD'], { cwd: managedSource, encoding: 'utf8' }).stdout.trim();
  const managedDigest = require('node:crypto').createHash('sha256').update(fs.readFileSync(managedBin)).digest('hex');
  fs.writeFileSync(path.join(managedSource, '.homeboy-runtime-identity.json'), JSON.stringify({ schema: 'homeboy/wp-codebox-managed-runtime-identity/v1', source_sha: managedSha, cli_sha256: managedDigest, required_capabilities: ['wp-codebox/browser-contained-site-open/v1'] }));
  await runWpCodeboxRecipe({
    recipeFile,
    artifactsDir,
    outputFile,
    env: { ...process.env, HOMEBOY_WP_CODEBOX_INSTALL_DIR: managedInstall, FIXTURE_CAPTURE_PATH: capturePath },
  });

  fs.appendFileSync(managedBin, '\n// tampered\n');
  await assert.rejects(
    runWpCodeboxRecipe({ recipeFile, artifactsDir, outputFile, env: { ...process.env, HOMEBOY_WP_CODEBOX_INSTALL_DIR: managedInstall, FIXTURE_CAPTURE_PATH: capturePath } }),
    /wp_codebox_managed_source_identity_invalid/
  );

  fs.mkdirSync(path.join(managedInstall, 'source.update-lock'));
  await assert.rejects(
    runWpCodeboxRecipe({ recipeFile, artifactsDir, outputFile, env: { ...process.env, HOMEBOY_WP_CODEBOX_INSTALL_DIR: managedInstall, FIXTURE_CAPTURE_PATH: capturePath } }),
    /wp_codebox_managed_updating/
  );
  // An explicit pin remains usable while an unrelated managed update holds its
  // lock; it is still subjected to the exact-command capability preflight.
  await runWpCodeboxRecipe({
    recipeFile,
    artifactsDir,
    outputFile,
    wpCodeboxBin: fixtureBin,
    env: { ...process.env, HOMEBOY_WP_CODEBOX_INSTALL_DIR: managedInstall, FIXTURE_CAPTURE_PATH: capturePath },
  });
  fs.rmSync(path.join(managedInstall, 'source.update-lock'), { recursive: true });
  // The helper accepts the selector's alternate configured pin inputs too.
  await runWpCodeboxRecipe({
    recipeFile,
    artifactsDir,
    outputFile,
    runtime_bin: fixtureBin,
    env: { ...process.env, HOMEBOY_WP_CODEBOX_INSTALL_DIR: managedInstall, FIXTURE_CAPTURE_PATH: capturePath },
  });

  await assert.rejects(
    runWpCodeboxRecipe({
      recipeFile,
      artifactsDir,
      outputFile,
      wpCodeboxBin: fixtureBin,
      env: { ...process.env, FIXTURE_CAPTURE_PATH: capturePath, FIXTURE_FAIL: '1' },
      event: (source, name, data) => events.push({ source, name, data }),
    }),
    /Command failed/
  );
  assert.equal(events.at(-1).name, 'recipe.failed');
  assert.deepEqual(JSON.parse(fs.readFileSync(outputFile, 'utf8')), { ok: false, failure: true });

  const largeOutputFile = path.join(root, 'output', 'wp-codebox-large-output.json');
  const largeResult = await runWpCodeboxRecipe({
    recipeFile,
    artifactsDir,
    outputFile: largeOutputFile,
    wpCodeboxBin: fixtureBin,
    maxBuffer: 1024,
    env: { ...process.env, FIXTURE_CAPTURE_PATH: capturePath, FIXTURE_LARGE_STDOUT: '1' },
  });
  assert.equal(largeResult.json.ok, true);
  assert.equal(largeResult.json.payload.length, 4096);
  assert.equal(JSON.parse(fs.readFileSync(largeOutputFile, 'utf8')).payload.length, 4096);

  // Abort: a wedged child (and its sub-process group) must be SIGTERM'd and
  // reaped, and the helper must reject with a clear killed error — no orphan.
  {
    const pidsPath = path.join(root, 'abort-pids.json');
    const controller = new AbortController();
    const runPromise = runWpCodeboxRecipe({
      recipeFile,
      artifactsDir,
      outputFile: path.join(root, 'output', 'abort-output.json'),
      wpCodeboxBin: fixtureBin,
      signal: controller.signal,
      killGraceMs: 100,
      env: { ...process.env, FIXTURE_CAPTURE_PATH: capturePath, FIXTURE_HANG: '1', FIXTURE_PIDS_PATH: pidsPath },
    });
    let pids = null;
    for (let i = 0; i < 200 && !pids; i += 1) {
      if (fs.existsSync(pidsPath)) {
        pids = JSON.parse(fs.readFileSync(pidsPath, 'utf8'));
      } else {
        await new Promise((resolve) => setTimeout(resolve, 25));
      }
    }
    assert.ok(pids && typeof pids.child === 'number', 'fixture should report its pid');
    assert.ok(pidAlive(pids.child), 'child should be running before abort');
    controller.abort();
    const abortError = await runPromise.then(
      () => { throw new Error('expected aborted recipe-run to reject'); },
      (error) => error
    );
    assert.match(abortError.message, /was aborted and was killed/);
    assert.equal(abortError.killed, true);
    assert.equal(abortError.killReason, 'abort');
    assert.equal(await waitForReaped([pids.child, pids.grandchild]), true, 'child + grandchild must be reaped after abort');
  }

  // Timeout: honoring timeoutMs alone (no external signal) must kill+reap, and a
  // child that ignores SIGTERM must still be SIGKILL'd after the grace window.
  {
    const pidsPath = path.join(root, 'timeout-pids.json');
    const timeoutError = await runWpCodeboxRecipe({
      recipeFile,
      artifactsDir,
      outputFile: path.join(root, 'output', 'timeout-output.json'),
      wpCodeboxBin: fixtureBin,
      timeoutMs: 150,
      killGraceMs: 100,
      env: { ...process.env, FIXTURE_CAPTURE_PATH: capturePath, FIXTURE_HANG: '1', FIXTURE_IGNORE_SIGTERM: '1', FIXTURE_PIDS_PATH: pidsPath },
    }).then(
      () => { throw new Error('expected timed-out recipe-run to reject'); },
      (error) => error
    );
    assert.match(timeoutError.message, /timed out after 150ms and was killed/);
    assert.equal(timeoutError.killed, true);
    assert.equal(timeoutError.killReason, 'timeout');
    assert.equal(timeoutError.timeout_ms, 150);
    const pids = JSON.parse(fs.readFileSync(pidsPath, 'utf8'));
    assert.equal(await waitForReaped([pids.child, pids.grandchild]), true, 'SIGTERM-ignoring child must be SIGKILL-reaped after grace');
  }

  console.log('wp-codebox recipe helper smoke passed');
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
