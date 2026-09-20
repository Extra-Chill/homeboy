# External Check Gate Handoff

Homeboy accepts provider-neutral CI publications through the controller event
primitive. A provider adapter owns API authentication, pagination, status
normalization, and hydration; the controller does not call GitHub or another
provider.

## Cook CI Mode

Opt into the two-phase Cook lifecycle with `--ci-mode` and the exact controller
identity fields:

```text
homeboy agent-task cook ... --ci-mode \
  --ci-loop-id LOOP_ID --ci-gate-id GATE_ID --ci-check-id CHECK_ID \
  --ci-environment-digest sha256:ENVIRONMENT
```

Cook first runs its normal local promotion gates, commits and pushes the exact
candidate, and creates or reuses a draft PR. It then returns
`awaiting_provider_ci` and persists `homeboy/cook-provider-ci-handoff/v1` with
the PR publication and candidate identity. The provider adapter hydrates the
exact PR head checks and applies `external.checks_changed` through the
controller. Re-run the durable Cook continuation after the event is applied.

The built-in GitHub adapter is invoked with the exact binding:

```text
homeboy review ci handoff --repo OWNER/REPO --pr PR_NUMBER \
  --loop-id LOOP_ID --gate-id GATE_ID --check-id CHECK_ID \
  --base-sha BASE_SHA --head-sha HEAD_SHA \
  --environment-digest sha256:ENVIRONMENT
```

It authenticates through `gh`, resolves the repository, PR, gate, check,
environment, base, and head identity from the durable Cook handoff, and rejects
caller arguments that do not match that authority. It verifies the live PR base
and head SHAs, paginates check runs for that exact head, and selects the newest
matching rerun before publishing only hydrated GitHub evidence. Pending results
are safe to repeat; the controller rejects stale, duplicate, out-of-order, and
mismatched publications.

Pending or mismatched evidence keeps the Cook in flight. A matching terminal
success transitions the same draft PR to ready-for-review. A failed terminal
publication dispatches one budgeted Cook remediation attempt keyed by the
candidate and evidence identity. Replays do not spend another provider
execution, and the replacement candidate receives a fresh CI identity; no
local gate is replayed merely to consume provider CI.

Publish an `external.checks_changed` event with a `publication` object using
`homeboy/external-check-publication/v1`. The publication must be authoritative
and hydrated and must include the repository, base and head commits, gate and
check IDs, environment digest, monotonic sequence, evidence ID, observed time,
status, and conclusion where the provider reports one.

The declared gate check must contain the same identity fields in its input.
Homeboy ignores missing, malformed, stale, duplicate, out-of-order, or
identity-mismatched publications. A check is accepted only for terminal
`success` or `completed` with `conclusion: success`; completed failures,
cancellations, timeouts, action-required, neutral, and skipped results remain
blocking failures. Queued and running states remain pending.

After a newer successful publication is accepted, `controller run-next`
consumes the durable gate result. It does not rerun the local command gate.
This handoff is only usable when an authenticated provider adapter is wired to
publish the event; this repository does not claim to publish GitHub events by
itself.
