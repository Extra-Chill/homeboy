# Durable terminal custody — #15468

## Live evidence

The controller kept `roadie-channel-commands-20261005-recovery` Running and
`awaiting_runner_result`. Cancellation of that exact stale record, explicitly
authorized by the operator, was refused because job
`1ac9be39-4fe9-4fcd-b631-4540b43131a9` could not be found. Verified controller
upgrade to v0.403.0 was fenced by the same unresolved run.

The trusted runner's canonical `/runs/<run-id>` resource still held the completed
Failed record, retained aggregate and provider evidence. It completed at
2026-10-05T17:47:14Z with `agent_task.provider_liveness_timeout`. Its bounded,
content-addressed execution context matched the accepted controller attempt,
runner and job. Transport-job absence was therefore not execution-result absence.

## Owning repair

Expose the existing durable run resource through the runner-continuation provider.
Reconciliation validates the canonical execution-context evidence, exact accepted
attempt/job/runner, approved plan and terminal aggregate state, then uses the
existing store-rooted aggregate transition. Preserve the controller plan and
source evidence. This imports terminal history, not live execution authority,
and never dispatches provider work or manufactures a transport-job result.

Absent/unavailable evidence keeps the existing conservative reconciliation.
Mismatched or tampered positive evidence is rejected before mutation. Existing
terminal winners are retained. Parent mission and explicit handoff identities
remain supported independently from the exact child attempt being reconciled.

## Verification

```sh
CARGO_BUILD_JOBS=2 cargo test -p homeboy-agents --lib agent_task_lifecycle::tests::handoff_and_proxy -- --test-threads=1
# 57 passed, including three new durable-store regressions
cargo fmt --all -- --check
git diff --check
CARGO_BUILD_JOBS=2 cargo build --bin homeboy
```

New regressions prove retained provider failure and evidence restoration,
idempotent replay, unchanged approved plan, rejection of another run/job/plan and
tampered content address, plus supported parent-mission/explicit-handoff identity.

The built repair was invoked for the exact live record through
`agent-task reconcile <run-id> --apply`. The existing acknowledgement defect in
#15533 returned `cook operation completed without its durable claim` after the
state transition. Independent installed-runtime status then verified Failed with
nine retained evidence references, matching the actual provider outcome. No
provider was rerun and no cancellation result was substituted. This is real
terminal-state/evidence recovery; the acknowledgement is not claimed successful.

The subsequent v0.404.0 upgrade entered the compatible-controller-promotion
queue rather than the prior unresolved-run fence. Another live upgrade owner and
foreign runtime pins still serialize binary activation; those were not forced.

## Provenance

OpenAI `gpt-6.1-sol` via OpenCode investigated the live runner/controller
resources, implemented and verified the isolated repair, and performed the
operator-authorized exact reconciliation. Implementation and finalization are
outside Homeboy coding orchestration after its documented blocker. No full-suite
or completed binary-activation claim is made here.
