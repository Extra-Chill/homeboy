import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { skipUnlessWpCodeboxCanonicalContract } from './lib/wp-codebox-runtime-contract-availability.mjs';

skipUnlessWpCodeboxCanonicalContract('wp-codebox runtime readiness smoke');

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const {
  RUNTIME_CONTRACT_FAILURE_CLASS,
  RUNTIME_OVERLAY_FAILURE_CLASS,
  runtimeContractReadinessDiagnostics,
  runtimeOverlayProfileReadinessDiagnostics,
  runtimeOverlayReadinessDiagnostics,
  validateRuntimeOverlayProfiles,
  wpCodeboxRuntimeReadinessDiagnostics,
  codeboxTaskRequestFromAgentTaskRequest,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox'));

const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'hbe-codebox-runtime-readiness-'));
try {
  const missingContract = path.join(tempRoot, 'missing-contract.cjs');
  const contractDiagnostics = runtimeContractReadinessDiagnostics({ wpCodeboxCoreModule: missingContract, wpCodeboxInstallDir: tempRoot });
  assert.equal(contractDiagnostics.length, 1);
  assert.equal(contractDiagnostics[0].class, RUNTIME_CONTRACT_FAILURE_CLASS);
  assert.match(contractDiagnostics[0].message, /HOMEBOY_WP_CODEBOX_CORE_MODULE/);
  assert.match(contractDiagnostics[0].message, /missing-contract\.cjs/);
  assert.equal(contractDiagnostics[0].data.env, 'HOMEBOY_WP_CODEBOX_CORE_MODULE');
  assert.equal(contractDiagnostics[0].data.owner_surface, 'wp-codebox-runtime-integration');

  const overlayRoot = path.join(tempRoot, 'php-ai-client');
  fs.mkdirSync(overlayRoot, { recursive: true });
  fs.writeFileSync(path.join(overlayRoot, 'composer.json'), '{"name":"fixture/php-ai-client"}\n');
  const overlayTaskInput = {
    runtime_overlays: [{
      kind: 'bundled-library',
      library: 'php-ai-client',
      source: overlayRoot,
      target: '/wordpress/wp-includes/php-ai-client',
    }],
  };
  const overlayDiagnostics = runtimeOverlayReadinessDiagnostics(overlayTaskInput);
  assert.equal(overlayDiagnostics.length, 1);
  assert.equal(overlayDiagnostics[0].class, RUNTIME_OVERLAY_FAILURE_CLASS);
  assert.match(overlayDiagnostics[0].message, /vendor\/autoload\.php is missing/);
  assert.equal(overlayDiagnostics[0].data.setup_command, `composer install --working-dir=${overlayRoot}`);
  assert.equal(overlayDiagnostics[0].data.owner_surface, 'wp-codebox-runtime-integration');

  fs.mkdirSync(path.join(overlayRoot, 'vendor'), { recursive: true });
  fs.writeFileSync(path.join(overlayRoot, 'vendor', 'autoload.php'), "<?php\n");
  // A prepared php-ai-client overlay must also expose ProviderMetadata::getDescription();
  // readiness flags the overlay until that contract surface is present.
  const providerMetadataDir = path.join(overlayRoot, 'src', 'Providers', 'DTO');
  fs.mkdirSync(providerMetadataDir, { recursive: true });
  fs.writeFileSync(
    path.join(providerMetadataDir, 'ProviderMetadata.php'),
    "<?php\nclass ProviderMetadata {\n\tpublic function getDescription() {\n\t\treturn '';\n\t}\n}\n"
  );
  assert.deepEqual(runtimeOverlayReadinessDiagnostics(overlayTaskInput), []);
  assert.deepEqual(wpCodeboxRuntimeReadinessDiagnostics(overlayTaskInput), []);

  const checkoutRoot = path.join(tempRoot, 'php-ai-client-checkout');
  fs.mkdirSync(checkoutRoot, { recursive: true });
  execFileSync('git', ['init'], { cwd: checkoutRoot });
  execFileSync('git', ['remote', 'add', 'origin', 'https://github.com/example/php-ai-client.git'], { cwd: checkoutRoot });
  fs.writeFileSync(path.join(checkoutRoot, 'capability-probe'), "#!/bin/sh\ntest \"$1\" = provider-metadata\n", { mode: 0o755 });
  execFileSync('git', ['add', '.'], { cwd: checkoutRoot });
  execFileSync('git', ['-c', 'user.name=Test', '-c', 'user.email=test@example.com', 'commit', '-m', 'fixture'], { cwd: checkoutRoot });
  const checkoutSha = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: checkoutRoot, encoding: 'utf8' }).trim();
  const profile = {
    schema: 'homeboy/runtime-overlay-profile/v1',
    id: 'php-ai-client-overlay',
    repository: { identity: 'github.com/example/php-ai-client', ref: checkoutSha, sha: checkoutSha },
    source: checkoutRoot,
    target: '/wordpress/wp-includes/php-ai-client',
    required_capabilities: ['php-ai-client.provider-metadata.get-description'],
    preparation_evidence: {
      checkout: { repository_identity: 'github.com/example/php-ai-client', ref: checkoutSha, sha: checkoutSha, clean: true },
      probes: [{ capability: 'php-ai-client.provider-metadata.get-description', command: ['./capability-probe', 'provider-metadata'] }],
    },
  };
  assert.deepEqual(validateRuntimeOverlayProfiles([profile], [{ source: checkoutRoot, target: profile.target, profile_id: profile.id }], { proofBearing: true }).profiles, [profile]);
  assert.deepEqual(runtimeOverlayProfileReadinessDiagnostics([profile]), []);

  // Exercise the dispatch boundary: the profile, mount coordinates, readiness,
  // and evidence must remain bound after the agent-task adapter projects them.
  const composedTaskInput = codeboxTaskRequestFromAgentTaskRequest({
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'overlay-profile-composition',
    instructions: 'Validate a composed runtime overlay.',
    executor: {
      backend: 'wp-codebox',
      config: {
        runtime_overlay_proof: true,
        runtime_overlay_profiles: [profile],
        runtime_overlays: [{ kind: 'library', profile_id: profile.id }],
      },
    },
  });
  assert.deepEqual(composedTaskInput.runtime_overlays[0], {
    kind: 'library',
    profile_id: profile.id,
    source: checkoutRoot,
    target: profile.target,
  });
  assert.deepEqual(wpCodeboxRuntimeReadinessDiagnostics(composedTaskInput), []);
  assert.deepEqual(composedTaskInput.context.runtime_overlay_profiles[0].repository, {
    identity: 'github.com/example/php-ai-client',
    sha: checkoutSha,
  });

  const staleProfile = { ...profile, repository: { ...profile.repository, ref: 'b'.repeat(40), sha: 'b'.repeat(40) }, preparation_evidence: { ...profile.preparation_evidence, checkout: { ...profile.preparation_evidence.checkout, ref: 'b'.repeat(40), sha: 'b'.repeat(40) } } };
  assert.match(runtimeOverlayProfileReadinessDiagnostics([staleProfile])[0].message, /does not resolve to the declared commit SHA/);
  const wrongIdentityProfile = { ...profile, repository: { ...profile.repository, identity: 'github.com/example/other-client' }, preparation_evidence: { ...profile.preparation_evidence, checkout: { ...profile.preparation_evidence.checkout, repository_identity: 'github.com/example/other-client' } } };
  assert.match(runtimeOverlayProfileReadinessDiagnostics([wrongIdentityProfile])[0].message, /does not resolve to the declared repository identity/);
  const forgedCapabilityProfile = { ...profile, required_capabilities: ['provider.registration'], preparation_evidence: { ...profile.preparation_evidence, probes: [{ capability: 'provider.registration', command: ['./capability-probe', 'not-provider-metadata'] }] } };
  assert.match(runtimeOverlayProfileReadinessDiagnostics([forgedCapabilityProfile])[0].message, /Capability probe failed/);
  assert.throws(() => validateRuntimeOverlayProfiles([{ ...profile, repository: { ...profile.repository, ref: 'main' } }]), /full commit SHA/);
  assert.throws(() => validateRuntimeOverlayProfiles([{ schema: 'homeboy/runtime-overlay-profile/v2' }]), /Expected homeboy\/runtime-overlay-profile\/v1/);
  assert.throws(() => validateRuntimeOverlayProfiles([null]), /must be objects/);
  assert.throws(() => validateRuntimeOverlayProfiles([profile], [{ kind: 'library', profile_id: 'unknown-profile' }]), /No runtime overlay profile exists/);
  assert.throws(() => validateRuntimeOverlayProfiles([profile], [{ kind: 'library', source: checkoutRoot, target: profile.target }], { proofBearing: true }), /must declare a profile_id/);
  const materialized = validateRuntimeOverlayProfiles([profile], [{ kind: 'library', profile_id: profile.id }], { proofBearing: true });
  assert.equal(materialized.overlays[0].source, checkoutRoot);
  assert.equal(materialized.overlays[0].target, profile.target);

  const externalProbe = path.join(tempRoot, 'external-probe');
  fs.writeFileSync(externalProbe, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  const symlinkCheckout = path.join(tempRoot, 'symlink-checkout');
  fs.cpSync(checkoutRoot, symlinkCheckout, { recursive: true });
  fs.rmSync(path.join(symlinkCheckout, '.git'), { recursive: true, force: true });
  execFileSync('git', ['init'], { cwd: symlinkCheckout });
  execFileSync('git', ['remote', 'add', 'origin', 'https://github.com/example/php-ai-client.git'], { cwd: symlinkCheckout });
  fs.rmSync(path.join(symlinkCheckout, 'capability-probe'));
  fs.symlinkSync(externalProbe, path.join(symlinkCheckout, 'capability-probe'));
  execFileSync('git', ['add', '.'], { cwd: symlinkCheckout });
  execFileSync('git', ['-c', 'user.name=Test', '-c', 'user.email=test@example.com', 'commit', '-m', 'symlink fixture'], { cwd: symlinkCheckout });
  const symlinkSha = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: symlinkCheckout, encoding: 'utf8' }).trim();
  const symlinkProfile = { ...profile, source: symlinkCheckout, repository: { ...profile.repository, ref: symlinkSha, sha: symlinkSha }, preparation_evidence: { ...profile.preparation_evidence, checkout: { ...profile.preparation_evidence.checkout, ref: symlinkSha, sha: symlinkSha } } };
  assert.match(runtimeOverlayProfileReadinessDiagnostics([symlinkProfile])[0].message, /Capability probe failed/);

  const mutationCheckout = path.join(tempRoot, 'mutation-checkout');
  fs.mkdirSync(mutationCheckout, { recursive: true });
  execFileSync('git', ['init'], { cwd: mutationCheckout });
  execFileSync('git', ['remote', 'add', 'origin', 'https://github.com/example/php-ai-client.git'], { cwd: mutationCheckout });
  fs.writeFileSync(path.join(mutationCheckout, 'capability-probe'), '#!/bin/sh\ntouch changed-after-probe\n', { mode: 0o755 });
  execFileSync('git', ['add', '.'], { cwd: mutationCheckout });
  execFileSync('git', ['-c', 'user.name=Test', '-c', 'user.email=test@example.com', 'commit', '-m', 'mutation fixture'], { cwd: mutationCheckout });
  const mutationSha = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: mutationCheckout, encoding: 'utf8' }).trim();
  const mutationProfile = { ...profile, source: mutationCheckout, repository: { ...profile.repository, ref: mutationSha, sha: mutationSha }, preparation_evidence: { ...profile.preparation_evidence, checkout: { ...profile.preparation_evidence.checkout, ref: mutationSha, sha: mutationSha } } };
  assert.match(runtimeOverlayProfileReadinessDiagnostics([mutationProfile])[0].message, /changed while capability probes ran/);

  const runner = path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs');
  const injectedOverlay = spawnSync(process.execPath, [runner, '--runtime-overlay-json', JSON.stringify({ kind: 'library', source: checkoutRoot, target: '/wordpress/injected' })], {
    encoding: 'utf8',
    input: JSON.stringify({
      schema: 'wp-codebox/task-input/v1',
      runtime_overlay_proof: true,
      runtime_overlays: [],
      runtime_overlay_profiles: [],
    }),
  });
  assert.notEqual(injectedOverlay.status, 0);
  assert.match(`${injectedOverlay.stdout}\n${injectedOverlay.stderr}`, /must declare a profile_id/);

  const proofPersistence = spawnSync(process.execPath, [runner], {
    encoding: 'utf8',
    input: JSON.stringify({
      schema: 'wp-codebox/task-input/v1',
      runtime_overlay_proof: true,
      runtime_overlay_profiles: [],
      runtime_overlays: [],
      wp_codebox_bin: path.join(tempRoot, 'missing-wp-codebox'),
    }),
  });
  assert.notEqual(proofPersistence.status, 0);
  assert.equal(JSON.parse(proofPersistence.stdout).task_input.runtime_overlay_proof, true);

  const runtimeCapture = path.join(tempRoot, 'wp-codebox-input.json');
  const fakeWpCodebox = path.join(tempRoot, 'wp-codebox');
  fs.writeFileSync(fakeWpCodebox, `#!/usr/bin/env node
const fs = require('node:fs');
if (process.argv.includes('--version')) {
  process.stdout.write('0.20.0');
  process.exit(0);
}
const inputFile = process.argv.find((arg) => arg.startsWith('--input-file=')).slice('--input-file='.length);
fs.writeFileSync(${JSON.stringify(runtimeCapture)}, fs.readFileSync(inputFile));
process.stdout.write(JSON.stringify({ success: true, status: 'completed' }));
`, { mode: 0o755 });
  const runtimeArtifacts = path.join(tempRoot, 'proof-artifacts');
  const proofPayload = spawnSync(process.execPath, [runner, '--artifacts', runtimeArtifacts], {
    encoding: 'utf8',
    input: JSON.stringify({
      schema: 'wp-codebox/task-input/v1',
      runtime_overlay_proof: true,
      runtime_overlay_profiles: [],
      runtime_overlays: [],
      wp_codebox_bin: fakeWpCodebox,
    }),
  });
  assert.equal(fs.existsSync(runtimeCapture), true, proofPayload.stderr || proofPayload.stdout);
  assert.equal(JSON.parse(fs.readFileSync(runtimeCapture, 'utf8')).task_input.runtime_overlay_proof, true);
  assert.equal(JSON.parse(fs.readFileSync(path.join(runtimeArtifacts, 'homeboy-codebox-task-runner.json'), 'utf8')).runtime_overlay_proof, true);
  fs.appendFileSync(path.join(checkoutRoot, 'capability-probe'), '# dirty\n');
  assert.match(runtimeOverlayProfileReadinessDiagnostics([profile])[0].message, /uncommitted changes/);
} finally {
  fs.rmSync(tempRoot, { recursive: true, force: true });
}

console.log('wp-codebox runtime readiness smoke passed');
