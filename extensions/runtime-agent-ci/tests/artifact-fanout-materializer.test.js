'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  AGENT_TASK_PLAN_SCHEMA,
  materializeArtifactFanout,
  runArtifactFanout,
} = require('../lib/artifact-fanout-materializer');

const controllerInput = {
  schema: 'homeboy/agent-task-loop-command-input/v1',
  controller: {
    task_lineage: [
      {
        run_id: 'producer-run',
        outputs: {
          artifacts: {
            grouped_records: {
              items: [
                { id: 'a', owner: 'alpha', kind: 'css', message: 'first' },
                { id: 'b', owner: 'alpha', kind: 'css', message: 'second' },
                { id: 'c', owner: 'beta', kind: 'html', message: 'third' },
              ],
            },
          },
        },
      },
    ],
  },
};

const result = materializeArtifactFanout({
  controller_input: controllerInput,
  config: {
    schema: 'homeboy-extensions/artifact-fanout-materializer-config/v1',
    artifact: 'grouped_records',
    item_path: 'items',
    group_by: ['owner', 'kind'],
    plan_id: 'generic-artifact-fanout',
    task_request_template: {
      task_id: 'repair-{{group.key}}',
      executor: { backend: 'fixture' },
      instructions: 'Handle {{group.item_count}} item(s) for {{group.key}}.',
      inputs: {
        group_key: '{{group.key}}',
        item_ids: '{{group.item_ids}}',
        items: '{{group.items}}',
      },
    },
  },
});

assert.equal(result.schema, 'homeboy-extensions/artifact-fanout-materializer-result/v1');
assert.equal(result.plan.schema, AGENT_TASK_PLAN_SCHEMA);
assert.equal(result.plan.plan_id, 'generic-artifact-fanout');
assert.equal(result.item_count, 3);
assert.equal(result.group_count, 2);
assert.deepEqual(result.groups.map((group) => group.key), ['alpha:css', 'beta:html']);
assert.deepEqual(result.plan.tasks.map((task) => task.task_id), ['repair-alpha:css', 'repair-beta:html']);
assert.equal(result.plan.tasks[0].instructions, 'Handle 2 item(s) for alpha:css.');
assert.deepEqual(result.plan.tasks[0].inputs.item_ids, ['a', 'b']);
assert.equal(result.plan.tasks[0].executor.backend, 'fixture');
assert.equal(result.plan.tasks[0].metadata.fanout_item_count, 2);

assert.throws(() => materializeArtifactFanout({ config: { artifact: 'missing', requires_non_empty: true }, controller_input: controllerInput }), /did not produce any items/);

const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-artifact-fanout-test-'));
try {
  const fakeHomeboy = path.join(tmpRoot, 'fake-homeboy.cjs');
  fs.writeFileSync(fakeHomeboy, `#!/usr/bin/env node
const fs = require('node:fs');
const inputIndex = process.argv.indexOf('--input');
if (inputIndex === -1 || !fs.existsSync(process.argv[inputIndex + 1])) {
  process.exit(2);
}
process.stdout.write(JSON.stringify({ batch: { batch_id: 'fanout-batch-1' } }));
`);
  fs.chmodSync(fakeHomeboy, 0o755);
  const fanoutSubmitInput = {
    items: [{ id: 'a', owner: 'alpha', kind: 'css' }],
    config: {
      group_by: ['owner', 'kind'],
      plan_id: 'generic-artifact-fanout',
      task_request_template: {
        task_id: 'repair-{{group.key}}',
        executor: { backend: 'fixture' },
        instructions: 'Handle {{group.item_count}} item(s) for {{group.key}}.',
      },
    },
  };
  const submitted = runArtifactFanout({
    ...fanoutSubmitInput,
    mode: 'submit',
    homeboy_bin: fakeHomeboy,
    batch_id: 'fanout-batch-1',
  });
  assert.equal(submitted.status, 'submitted');
  assert.equal(submitted.plan_location.storage, 'scratch');
  assert.equal(submitted.plan_location.cleanup, 'caller');
  assert.equal(submitted.scratch.kind, 'caller-owned-temporary-directory');
  assert.equal(fs.existsSync(submitted.plan_path), true);

  const persistentDir = path.join(tmpRoot, 'persisted-plan');
  const persisted = runArtifactFanout({
    ...fanoutSubmitInput,
    mode: 'submit',
    homeboy_bin: fakeHomeboy,
    plan_dir: persistentDir,
  });
  assert.equal(persisted.plan_location.storage, 'persistent');
  assert.equal(persisted.plan_path, path.join(persistentDir, 'plan.json'));
  assert.equal(Object.prototype.hasOwnProperty.call(persisted, 'scratch'), false);
} finally {
  fs.rmSync(tmpRoot, { recursive: true, force: true });
}

console.log('artifact fanout materializer ok');
