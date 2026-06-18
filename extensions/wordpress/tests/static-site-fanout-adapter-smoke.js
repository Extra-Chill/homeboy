'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {
  createStaticSiteFanoutPlan,
  executeStaticSiteFanout,
  normalizeArtifactRefs,
} = require('../lib/static-site-fanout-adapter');

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

async function main() {
  const findings = readJson(path.join(__dirname, 'fixtures', 'static-site-fanout-adapter', 'finding-packets.json'));
  const plan = createStaticSiteFanoutPlan({
    run_id: 'fixture-run',
    parent_plan_id: 'fixture-parent-plan',
    provider: 'codex',
    model: 'gpt-5.5',
    secret_env: ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN'],
    findings,
    agent_task: {
      source: 'bundles/php-transformer-iterator-agent',
      agent_slug: 'php-transformer-iterator-agent',
      pipeline_slug: 'php-transformer-iterator-pipeline',
      flow_slug: 'php-transformer-iterator-manual-flow',
    },
  });
  const goldenPlan = readJson(path.join(__dirname, 'fixtures', 'static-site-fanout-adapter', 'golden-agent-plan-summary.json'));
  assert.deepEqual(projectPlan(plan), goldenPlan);
  assert.equal(plan.task_requests[0].schema, 'homeboy/agent-task-request/v1');
  assert.equal(plan.task_requests[0].executor.backend, 'codebox');
  assert.equal(plan.task_requests[0].inputs.finding_ids.length, 2);
  assert.equal(plan.task_requests[0].inputs.artifact_refs[0].kind, 'diagnostic');

  const genericBackendPlan = createStaticSiteFanoutPlan({
    run_id: 'generic-backend-run',
    backend: 'opencode',
    findings: [findings[0]],
  });
  assert.equal(genericBackendPlan.orchestrator.backend, 'opencode');
  assert.equal(genericBackendPlan.task_requests[0].executor.backend, 'opencode');

  const codeboxPlan = createStaticSiteFanoutPlan({
    run_id: 'fixture-run',
    request_kind: 'wp-codebox',
    groups: [
      {
        group_key: 'visual-parity',
        findings: [findings[2]],
        artifact_refs: [{ artifact_id: 'visual-diff.json', kind: 'visual_parity_artifact', path: 'artifacts/visual-diff.json' }],
      },
    ],
  });
  assert.equal(codeboxPlan.task_requests[0].schema, 'wp-codebox/task-input/v1');
  assert.equal(codeboxPlan.task_requests[0].context.findings[0].id, 'visual-parity-demo-store');
  assert.equal(codeboxPlan.task_requests[0].context.artifact_refs[0].kind, 'visual_parity_artifact');

  const emptyPlan = createStaticSiteFanoutPlan({ run_id: 'empty-run', findings: [] });
  assert.equal(emptyPlan.static_site.no_actionable_findings, true);
  assert.equal(emptyPlan.task_requests.length, 0);
  assert.equal(emptyPlan.reconciliation.status, 'no_actionable_findings');

  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-static-site-fanout-adapter-'));
  const runsOutputPath = path.join(root, 'run.json');
  const progressEvents = [];
  try {
    const run = await executeStaticSiteFanout({
      plan,
      concurrency: 2,
      runsOutputPath,
      on_progress: (event) => progressEvents.push(event),
      execute_task_request: async (taskRequest) => ({
        task_id: taskRequest.task_id,
        group_key: taskRequest.group_key,
        status: 'completed',
        outcome: {
          kind: 'artifact_refs',
          group_key: taskRequest.group_key,
          artifact_refs: [
            { artifact_id: `issue-${taskRequest.group_key}`, kind: 'issue', url: `https://github.com/example/repo/issues/${taskRequest.group_key === 'converter-support' ? '1' : '2'}` },
          ],
        },
      }),
    });

    assert.equal(run.schema, 'homeboy/static-site-fanout-run/v1');
    assert.equal(run.status, 'completed');
    assert.equal(run.reconciliation.task_count, 2);
    assert.equal(run.reconciliation.artifact_refs.length, 5);
    assert.equal(progressEvents.filter((event) => event.status === 'started').length, 2);
    assert.equal(readJson(runsOutputPath).reconciliation.artifact_refs.length, 5);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }

  assert.deepEqual(normalizeArtifactRefs(['diag-1', { artifact_id: 'diag-1', kind: 'artifact' }]), [
    { schema: 'homeboy/artifact-ref/v1', artifact_id: 'diag-1', kind: 'artifact', path: '', url: '', source: '', role: '' },
  ]);

  console.log('Homeboy static-site fanout adapter smoke passed');
}

function projectPlan(plan) {
  return {
    schema: plan.schema,
    orchestrator: plan.orchestrator,
    summary: plan.summary,
    groups: plan.groups,
    task_requests: plan.task_requests.map((request) => ({
      schema: request.schema,
      task_id: request.task_id,
      group_key: request.group_key,
      parent_plan_id: request.parent_plan_id,
      executor_backend: request.executor?.backend,
      executor_secret_env: request.executor?.secret_env || [],
      runtime_task: request.executor?.config?.runtime_task,
      instructions: request.instructions,
      inputs: request.inputs,
      limits: request.limits,
      expected_artifacts: request.expected_artifacts,
    })),
    reconciliation: plan.reconciliation,
    static_site: plan.static_site,
  };
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
