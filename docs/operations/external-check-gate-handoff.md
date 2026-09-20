# External Check Gate Handoff

Homeboy accepts provider-neutral CI publications through the controller event
primitive. A provider adapter owns API authentication, pagination, status
normalization, and hydration; the controller does not call GitHub or another
provider.

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
