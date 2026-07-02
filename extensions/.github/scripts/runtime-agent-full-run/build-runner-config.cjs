#!/usr/bin/env node
'use strict';

const {
  buildConfig,
  buildSecretEnvFallbacks,
  buildSecretEnvPlan,
  loopPolicyFromEnv,
  projectRuntimeConfig,
  providerBenchEnvFromManifest,
  runtimePathRequired,
  writeFullRunConfig,
  workflowInputCompatibility,
} = require('../../../runtime-agent-ci/provider-adapters');

function main() {
  writeFullRunConfig(process.env);
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}

module.exports = { buildConfig, buildSecretEnvFallbacks, buildSecretEnvPlan, loopPolicyFromEnv, projectRuntimeConfig, providerBenchEnvFromManifest, runtimePathRequired, workflowInputCompatibility, writeFullRunConfig };
