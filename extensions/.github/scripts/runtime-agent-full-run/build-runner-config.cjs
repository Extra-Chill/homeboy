#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {
  buildConfig,
  loopPolicyFromEnv,
  projectRuntimeConfig,
  providerBenchEnvFromManifest,
  runtimePathRequired,
  withoutInternalKeys,
} = require('../../../runtime-agent-ci');
const { writeGithubOutput } = require('../../../runtime-agent-ci/lib/full-run-inputs.cjs');

function main() {
  const config = buildConfig(process.env);
  fs.mkdirSync(path.dirname(config._configPath), { recursive: true });
  fs.writeFileSync(config._configPath, `${JSON.stringify(withoutInternalKeys(config), null, 2)}\n`);
  writeGithubOutput({ config_path: config._configPath, transcript_host_dir: config.transcript_host_dir });
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}

module.exports = { buildConfig, loopPolicyFromEnv, projectRuntimeConfig, providerBenchEnvFromManifest, runtimePathRequired };
