'use strict';

// Generator for `homeboy-agent-task-core-contract.json`.
//
// The fixture is the cross-repo handoff contract for durable host
// orchestration (see ../../docs/agent-runtime-package-contract.md). It is the
// union of two single-source-of-truth surfaces:
//
//   1. Homeboy core's authoritative agent-task core contract, published by the
//      `homeboy agent-task contract --format json` command. This generator
//      consumes that command output directly so the core region of the fixture
//      can never silently drift from core's `agent_task_core_contract()`.
//   2. The extensions-owned fanout/reconcile + orchestration overlay, derived
//      from the same JS constant modules the runtime uses at execution time.
//
// Usage:
//   node generate-homeboy-agent-task-core-contract.cjs            # write fixture
//   node generate-homeboy-agent-task-core-contract.cjs --check    # verify only
//
// `--check` regenerates from core + overlay and fails (exit 1) when the
// committed fixture differs, so CI catches drift without hand-maintenance.

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const {
  AGENT_TASK_OUTCOME_STATUSES,
} = require('../../agent-task-contracts');
const {
  FANOUT_RECONCILE_PLAN_SCHEMA,
  FANOUT_RECONCILE_RECORD_STATUSES,
  FANOUT_RECONCILE_RUN_SCHEMA,
  FANOUT_RECONCILE_RUN_STATUSES,
} = require('../../runtime-agent-ci/lib/fanout-reconcile-runner');
const {
  GENERIC_FANOUT_RECONCILE_CONFIG_SCHEMA,
  GENERIC_FANOUT_RECONCILE_RECONCILIATION_SCHEMA,
  GENERIC_FANOUT_RECONCILE_RESULT_SCHEMA,
  GENERIC_FANOUT_RECONCILE_SUCCESS_STATUSES,
  GENERIC_FINDING_PACKET_FANOUT_CONFIG_SCHEMA,
} = require('../../runtime-agent-ci/lib/generic-fanout-reconcile-workflow');

const FIXTURE_PATH = path.join(__dirname, 'homeboy-agent-task-core-contract.json');

// Outcome statuses that map to a successful host-orchestration record status.
// Every other agent-task outcome status maps to `failed` (see the bridge table
// in docs/agent-runtime-package-contract.md).
const FANOUT_RECORD_SUCCESS_OUTCOME_STATUSES = ['succeeded', 'no_op'];

// Extensions-owned overlay merged on top of the core contract. These keys are
// additive: the host orchestration runtime owns fanout/reconcile, while the
// core region remains owned by Homeboy core.
function extensionsOverlay() {
  const outcomeToRecordStatus = {};
  for (const status of AGENT_TASK_OUTCOME_STATUSES) {
    outcomeToRecordStatus[status] = FANOUT_RECORD_SUCCESS_OUTCOME_STATUSES.includes(status)
      ? 'completed'
      : 'failed';
  }

  return {
    schemas: {
      fanout_reconcile_config: GENERIC_FANOUT_RECONCILE_CONFIG_SCHEMA,
      fanout_reconcile_plan: FANOUT_RECONCILE_PLAN_SCHEMA,
      fanout_reconcile_run: FANOUT_RECONCILE_RUN_SCHEMA,
      fanout_reconcile_result: GENERIC_FANOUT_RECONCILE_RESULT_SCHEMA,
      fanout_reconcile_reconciliation: GENERIC_FANOUT_RECONCILE_RECONCILIATION_SCHEMA,
      finding_packet_fanout_config: GENERIC_FINDING_PACKET_FANOUT_CONFIG_SCHEMA,
    },
    enums: {
      fanout_record_status: [...FANOUT_RECONCILE_RECORD_STATUSES],
      fanout_run_status: [...FANOUT_RECONCILE_RUN_STATUSES],
    },
    orchestration: {
      agent_task_outcome_to_fanout_record_status: outcomeToRecordStatus,
      fanout_success_statuses: [...GENERIC_FANOUT_RECONCILE_SUCCESS_STATUSES],
    },
  };
}

// Run `homeboy agent-task contract --format json` and return the contract body
// (`.data`). Returns null when the homeboy binary is unavailable so callers can
// degrade gracefully in environments without a Homeboy install.
function fetchCoreContractData(options = {}) {
  const binary = options.binary || process.env.HOMEBOY_BIN || 'homeboy';
  const result = spawnSync(binary, ['agent-task', 'contract', '--format', 'json'], {
    encoding: 'utf8',
  });
  if (result.error) {
    if (result.error.code === 'ENOENT') {
      return null;
    }
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(
      `\`${binary} agent-task contract --format json\` exited ${result.status}: ${result.stderr || ''}`
    );
  }
  const envelope = JSON.parse(result.stdout);
  if (!envelope || envelope.success !== true || typeof envelope.data !== 'object') {
    throw new Error('Unexpected homeboy agent-task contract envelope shape');
  }
  return envelope.data;
}

// Merge the core contract with the extensions overlay. Core keys are
// authoritative; overlay keys are additive within `schemas`/`enums` and add the
// top-level `orchestration` section.
function buildCoreContractFixture(coreData) {
  assert.ok(coreData && typeof coreData === 'object', 'core contract data is required');
  const overlay = extensionsOverlay();
  return {
    ...coreData,
    schemas: { ...coreData.schemas, ...overlay.schemas },
    enums: { ...coreData.enums, ...overlay.enums },
    orchestration: overlay.orchestration,
  };
}

// Deterministic serialization: recursively sort object keys (arrays keep their
// order, which is semantic), 2-space indent, trailing newline.
function canonicalJson(value) {
  return `${JSON.stringify(sortKeysDeep(value), null, 2)}\n`;
}

function sortKeysDeep(value) {
  if (Array.isArray(value)) {
    return value.map(sortKeysDeep);
  }
  if (value && typeof value === 'object') {
    return Object.keys(value)
      .sort()
      .reduce((sorted, key) => {
        sorted[key] = sortKeysDeep(value[key]);
        return sorted;
      }, {});
  }
  return value;
}

function readCommittedFixture() {
  return fs.readFileSync(FIXTURE_PATH, 'utf8');
}

function main(argv) {
  const check = argv.includes('--check');
  const coreData = fetchCoreContractData();
  if (coreData === null) {
    process.stderr.write(
      'homeboy binary not found; cannot regenerate the agent-task core contract fixture.\n'
        + 'Install Homeboy or set HOMEBOY_BIN to a Homeboy executable.\n'
    );
    process.exit(check ? 0 : 1);
    return;
  }

  const generated = canonicalJson(buildCoreContractFixture(coreData));

  if (check) {
    const committed = readCommittedFixture();
    if (committed !== generated) {
      process.stderr.write(
        'Drift detected: homeboy-agent-task-core-contract.json is out of sync with core.\n'
          + 'Regenerate with: node agent-runtimes/fixtures/generate-homeboy-agent-task-core-contract.cjs\n'
      );
      process.exit(1);
      return;
    }
    process.stdout.write('Agent task core contract fixture is in sync with core.\n');
    return;
  }

  fs.writeFileSync(FIXTURE_PATH, generated);
  process.stdout.write(`Wrote ${path.relative(process.cwd(), FIXTURE_PATH)}\n`);
}

module.exports = {
  FIXTURE_PATH,
  buildCoreContractFixture,
  canonicalJson,
  extensionsOverlay,
  fetchCoreContractData,
  sortKeysDeep,
};

if (require.main === module) {
  main(process.argv.slice(2));
}
