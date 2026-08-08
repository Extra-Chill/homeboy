#!/usr/bin/env bash

set -euo pipefail

config="${CI_CAPACITY_CONFIG:-.github/ci-capacity.json}"
jq -e '
  . as $config
  | .schema == "homeboy/ci-capacity/v1"
  and (.test_shards | type == "number" and floor == . and . >= 1)
  and (.queue_delay_slo_seconds.window_days | type == "number" and . >= 1)
  and (.queue_delay_slo_seconds.p95 | type == "number" and . > 0)
  and ($config.queue_delay_slo_seconds.p99 | type == "number" and . >= $config.queue_delay_slo_seconds.p95)
  and (.execution_slo_seconds.test_shard_p95 | type == "number" and . > 0)
  and ($config.execution_slo_seconds.required_critical_path_p95 | type == "number" and . >= $config.execution_slo_seconds.test_shard_p95)
' "${config}" >/dev/null

shards="$(jq -r '.test_shards' "${config}")"
queue_p95="$(jq -r '.queue_delay_slo_seconds.p95' "${config}")"
queue_p99="$(jq -r '.queue_delay_slo_seconds.p99' "${config}")"
window_days="$(jq -r '.queue_delay_slo_seconds.window_days' "${config}")"
shard_p95="$(jq -r '.execution_slo_seconds.test_shard_p95' "${config}")"
critical_p95="$(jq -r '.execution_slo_seconds.required_critical_path_p95' "${config}")"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "test-shards=${shards}"
    echo "queue-delay-p95-seconds=${queue_p95}"
    echo "queue-delay-p99-seconds=${queue_p99}"
    echo "test-shard-p95-seconds=${shard_p95}"
    echo "required-critical-path-p95-seconds=${critical_p95}"
  } >> "${GITHUB_OUTPUT}"
fi

cat <<EOF >> "${GITHUB_STEP_SUMMARY:?GITHUB_STEP_SUMMARY is required}"
## CI capacity admission

| Field | Value |
| --- | --- |
| State | admitted |
| Requested deterministic Test shards | ${shards} |
| Admitted Test shards | ${shards} |
| Deferred Test shards | 0 |
| Capacity budget | ${shards} concurrent Test shards per run |
| Queue-delay SLO | ${window_days}-day p95 <= ${queue_p95}s; p99 <= ${queue_p99}s |
| Execution SLO | Test shard p95 <= ${shard_p95}s; required-check critical path p95 <= ${critical_p95}s |

This repository admits only the configured shard budget. GitHub-hosted runner availability remains separately visible in the timing evidence job.
EOF
