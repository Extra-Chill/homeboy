'use strict';

// Generates the core-owned provider vocabulary consumed by extension runtimes
// from the pinned core contract fixture. Runtime-specific tool and capability
// policy remains in agent-task-provider-contract.js.

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const OUTPUT_PATH = path.join(__dirname, 'agent-task-provider-contract.generated.json');
const CORE_CONTRACT_PATH = path.join(
  __dirname,
  '..',
  'agent-runtimes',
  'fixtures',
  'homeboy-agent-task-core-contract.json'
);
const CORE_CONTRACT_SCHEMA = 'homeboy/agent-task-core-contract/v1';
const GENERATED_SCHEMA = 'homeboy/agent-task-provider-contract-export/v1';

function readPinnedCoreContract() {
  return JSON.parse(fs.readFileSync(CORE_CONTRACT_PATH, 'utf8'));
}

function buildProviderContract(coreContract) {
  assert.equal(coreContract?.schema, CORE_CONTRACT_SCHEMA, 'Unexpected core agent-task contract schema');
  const providerCapability = coreContract.provider_capability;
  assert.ok(providerCapability && typeof providerCapability === 'object', 'Core provider capability contract is required');

  return {
    _generated_by: 'agent-runtimes/fixtures/homeboy-agent-task-core-contract.json',
    core_contract_schema: coreContract.schema,
    provider_capability: {
      failure_classifications: providerCapability.failure_classifications,
      outcome_statuses: providerCapability.outcome_statuses,
      redacted_metadata_keys: providerCapability.redacted_metadata_keys,
      request_required_fields: providerCapability.request_required_fields,
    },
    schema: GENERATED_SCHEMA,
    schemas: {
      artifact: coreContract.schemas.artifact,
      artifact_declaration: coreContract.schemas.artifact_declaration,
      evidence_ref: coreContract.schemas.evidence_ref,
      outcome: coreContract.schemas.outcome,
      provider: coreContract.schemas.provider,
      request: coreContract.schemas.request,
      secret_env_requirement: coreContract.schemas.secret_env_requirement,
    },
  };
}

function canonicalJson(value) {
  return `${JSON.stringify(sortKeysDeep(value), null, 2)}\n`;
}

function sortKeysDeep(value) {
  if (Array.isArray(value)) {
    return value.map(sortKeysDeep);
  }
  if (value && typeof value === 'object') {
    return Object.keys(value).sort().reduce((sorted, key) => {
      sorted[key] = sortKeysDeep(value[key]);
      return sorted;
    }, {});
  }
  return value;
}

function main(argv) {
  const generated = canonicalJson(buildProviderContract(readPinnedCoreContract()));
  if (argv.includes('--check')) {
    if (fs.readFileSync(OUTPUT_PATH, 'utf8') !== generated) {
      throw new Error(`Drift detected: regenerate with node ${path.relative(process.cwd(), __filename)}`);
    }
    process.stdout.write('Agent task provider contract is in sync with core.\n');
    return;
  }
  fs.writeFileSync(OUTPUT_PATH, generated);
  process.stdout.write(`Wrote ${path.relative(process.cwd(), OUTPUT_PATH)}\n`);
}

module.exports = {
  CORE_CONTRACT_PATH,
  CORE_CONTRACT_SCHEMA,
  GENERATED_SCHEMA,
  OUTPUT_PATH,
  buildProviderContract,
  canonicalJson,
  readPinnedCoreContract,
};

if (require.main === module) {
  main(process.argv.slice(2));
}
