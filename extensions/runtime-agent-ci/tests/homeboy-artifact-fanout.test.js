'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = path.resolve(__dirname, '..');
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-artifact-fanout-'));

try {
  const configPath = path.join(tmp, 'config.json');
  const inputPath = path.join(tmp, 'input.json');
  const outputPath = path.join(tmp, 'output.json');
  fs.writeFileSync(configPath, JSON.stringify({
    schema: 'homeboy-extensions/artifact-fanout-materializer-config/v1',
    artifact: 'finding_group',
    item_path: 'items',
    group_by: ['owner_repo', 'root_cause'],
    requires_non_empty: true,
    plan_id: 'fixture-plan',
    batch_id: '{{env.HOMEBOY_LOOP_ID}}-{{env.HOMEBOY_LOOP_ACTION_ID}}',
    output_artifact: 'iterator_fanout_batch',
    task_request_template: {
      task_id: 'task-{{group.key}}',
      inputs: { finding_group: '{{group.items}}' },
    },
  }));
  fs.writeFileSync(inputPath, JSON.stringify({
    request: {
      inputs: {
        artifacts: {
          finding_group: {
            payload: {
              items: [
                { id: 'a', owner_repo: 'repo-a', root_cause: 'cause-1' },
                { id: 'b', owner_repo: 'repo-a', root_cause: 'cause-1' },
              ],
            },
          },
        },
      },
    },
  }));

  const result = spawnSync(process.execPath, [path.join(root, 'scripts/homeboy-artifact-fanout.cjs'), '--config', configPath], {
    cwd: root,
    encoding: 'utf8',
    env: {
      ...process.env,
      HOMEBOY_LOOP_ID: 'loop-1',
      HOMEBOY_LOOP_ACTION_ID: 'action-2',
      HOMEBOY_LOOP_ACTION_INPUT: inputPath,
      HOMEBOY_LOOP_ACTION_OUTPUT: outputPath,
    },
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = JSON.parse(fs.readFileSync(outputPath, 'utf8'));
  const batch = output.artifacts.iterator_fanout_batch;
  assert.equal(batch.schema, 'homeboy-extensions/artifact-fanout-materializer-result/v1');
  assert.equal(batch.item_count, 2);
  assert.equal(batch.group_count, 1);
  assert.equal(batch.plan.tasks[0].task_id, 'task-repo-a:cause-1');
  assert.deepEqual(batch.plan.tasks[0].inputs.finding_group.map((item) => item.id), ['a', 'b']);
} finally {
  fs.rmSync(tmp, { recursive: true, force: true });
}

console.log('homeboy artifact fanout test passed');
