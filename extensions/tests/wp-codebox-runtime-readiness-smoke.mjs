import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
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
  runtimeOverlayReadinessDiagnostics,
  wpCodeboxRuntimeReadinessDiagnostics,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox'));

const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'hbe-codebox-runtime-readiness-'));
try {
  const missingContract = path.join(tempRoot, 'missing-contract.cjs');
  const contractDiagnostics = runtimeContractReadinessDiagnostics({ wpCodeboxCoreModule: missingContract });
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
} finally {
  fs.rmSync(tempRoot, { recursive: true, force: true });
}

console.log('wp-codebox runtime readiness smoke passed');
