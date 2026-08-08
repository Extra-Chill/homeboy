#!/usr/bin/env bash

set -euo pipefail

config="${CI_CAPACITY_CONFIG:-.github/ci-capacity.json}"
run="$(gh api "repos/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}")"
jobs="$(gh api --paginate "repos/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}/jobs?per_page=100" | jq -s '{jobs: map(.jobs[]) }')"
required='["homeboy / Required Gates Declaration","homeboy / Workspace Tests Compile","homeboy / Windows Compile","homeboy / Rustfmt","homeboy / Audit","homeboy / Lint","homeboy / Test"]'

jq -n --argjson run "${run}" --argjson jobs "${jobs}" --argjson required "${required}" --slurpfile config "${config}" '
  def seconds($from; $to): (($to | fromdateiso8601) - ($from | fromdateiso8601));
  ($jobs.jobs | map(select(.started_at != null and .completed_at != null))) as $completed
  | ($completed | map({name, queue_delay_seconds: seconds(.created_at; .started_at), execution_seconds: seconds(.started_at; .completed_at)})) as $timings
  | ($completed | map(select(.name as $name | $required | index($name))) | max_by(.completed_at)) as $critical
  | {
      schema: "homeboy/ci-capacity-evidence/v1",
      run_id: $run.id,
      workflow_created_at: $run.created_at,
      admission: {state: "admitted", configured_test_shards: $config[0].test_shards, deferred_test_shards: 0},
      jobs: $timings,
      required_critical_path: (if $critical == null then null else {
        terminal_job: $critical.name,
        terminal_at: $critical.completed_at,
        created_to_terminal_seconds: seconds($run.created_at; $critical.completed_at)
      } end),
      slo: $config[0]
    }
' > ci-capacity-evidence.json

{
  echo "## CI queue and execution evidence"
  echo
  echo "The admission decision is configuration-backed. GitHub exposes no separate scheduler admission timestamp, so each job's \\`created -> runner started\\` value is the observed hosted-runner queue delay."
  echo
  echo '| Job | Queue delay (s) | Execution (s) |'
  echo '| --- | ---: | ---: |'
  jq -r '.jobs[] | "| \(.name) | \(.queue_delay_seconds) | \(.execution_seconds) |"' ci-capacity-evidence.json
  echo
  jq -r 'if .required_critical_path == null then "Required critical path was not terminal; see job states above." else "Required critical path: \(.required_critical_path.terminal_job), created -> terminal \(.required_critical_path.created_to_terminal_seconds)s." end' ci-capacity-evidence.json
  echo
  echo "Rolling SLO evaluation requires seven days of retained run evidence; this run publishes its raw timing record for that aggregation."
} >> "${GITHUB_STEP_SUMMARY:?GITHUB_STEP_SUMMARY is required}"
