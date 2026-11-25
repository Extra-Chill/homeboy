# Batch landing

`homeboy agent-task batch-land` is an opt-in, workflow-callable landing step
for already-published child Cooks. Existing `fanout cook-batch` behavior is
unchanged. Supply the published PR and durable Cook identity for every child:

```sh
homeboy agent-task batch-land \
  --child cook-one=https://github.com/OWNER/REPO/pull/1 \
  --child cook-two=https://github.com/OWNER/REPO/pull/2 \
  --required-check test --method squash --max-reruns 1 --max-updates 1
```

The same argument vector can be used as a workflow `homeboy` step after
publication. `--admin` explicitly permits administrator merges; it is off by
default. All reported checks must finish successfully (including neutral or
skipped conclusions); named required checks must exist. Empty rollups never
merge. Merges are pinned to the checked head SHA, which must match the Cook's
selected candidate.

A failure is eligible for a flake rerun only when check annotations identify
failures in test files the PR did not change. Missing or truncated evidence,
non-test failures and touched tests go to Cook feedback instead. This deliberately
does not guess test ownership from an arbitrary job name. Actions check the
configured lifetime budgets retained in the original child Cook's
`metadata.landing_events`; repeated invocations reuse those budgets.

Conflicts and non-flaky failures submit idempotent candidate feedback and admit
`agent-task recover` on the same Cook. No independent replacement Cook is created.
The command observes the PR while recovery runs. Unsupported repair admission,
remote command failures, exhausted budgets and observation deadlines produce an
explicit per-child `operator_reason` and a nonzero exit.

The JSON result includes every child outcome and a cross-PR `flake_ledger` with
test identity, occurrence count and PR URLs, reconstructed from durable events.
Actions are recorded before their remote effects. A crash in that window consumes
a budget rather than risking unbounded repeated mutations; inspect the recorded
intent and GitHub state before retrying a blocked child.

## Acceptance evidence

The required live three-child run with injected flake and conflict has **not**
been performed in this checkout. Local classifier tests do not substitute for
that run. A dedicated test repository, three runnable child Cook records and
provider/runner credentials are needed before claiming that acceptance result.
