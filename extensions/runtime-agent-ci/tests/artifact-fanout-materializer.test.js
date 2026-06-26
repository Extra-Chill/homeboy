'use strict';

const assert = require('node:assert/strict');

const {
  AGENT_TASK_PLAN_SCHEMA,
  materializeArtifactFanout,
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

console.log('artifact fanout materializer ok');
