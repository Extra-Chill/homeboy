# Cook Recovery

A maintainer's map of how Homeboy decides what to do with a stopped Cook. Operator
usage lives in [`homeboy agent-task`](../../commands/agent-task.md); this page
names the code that owns each decision.

## One decider

Code: `crates/homeboy-agents/src/agent_task_service/cook_recovery.rs`.

Every surface that names a stopped Cook's next step asks one function,
`cook_recovery_step`, through `plan_cook_recovery`:

- `agent-task status` and `diagnose` render it as the `Next:` action.
- The Cook verdict and `review` project it (`orchestration.rs`).
- `agent-task recover <cook> --preflight` reports it.
- `agent-task recover <cook>` performs it.

Because they share one answer, status can never advertise a step that
`recover` would not perform. Legal-action classification in
`cook_promotion.rs` derives its mutating action from the same step instead of
classifying separately. When you add a recovery path, add it to
`decide_cook_recovery_step`. Don't add a second check in a caller.

## Durable Cook state

`agent_task_lifecycle/cook_run_state.rs` owns Cook state writes. The internal
persisted state contains only the execution phase and replay receipts, not a
cached verdict. `CookRunState::of(record)` prefers that phase, then classifies
current observation evidence, continuation ownership, fencing and liveness on
top. Records without persisted state seed the phase from their execution state;
remove that migration fallback after all retained records have been rewritten.

The lifecycle store adapts terminalization, harvest, regate/recover, cancellation
and requeue at the shared revision-fenced commit boundary. Phase transitions
are validated against the stored execution phase, never an evidence-refined
read view. The phase and receipts commit atomically with the lifecycle changes.
`apply(event, key)` rejects illegal edges and key reuse for different events;
replaying an event/key is a no-op even after later events. Store adapters use
the next run revision as the per-run key. Unfenced test fixtures seed rather
than transition state.

Cook observations can refine provider success to `CandidateRecoverable` when
verification or finalization is incomplete, or to success when publication
receipts supersede older failure snapshots. That read-only refinement is passed
to the same classifier, not persisted as a competing phase. It cannot erase or
replace replay receipts. Thus persisted and legacy records use the same verdict
classification and observations never manufacture lifecycle transitions.

Ordinary record JSON omits both persisted state and transient observation facts,
preserving command output contracts. The private durable field requires an
authority token only the state module can create. The write-ownership guard
covers transient phase refinements too.

## Steps

`CookRecoveryStep` is the whole vocabulary:

| Step | When | What `recover` does |
| --- | --- | --- |
| `Continue` | A continuation is admitted (also a disconnected runner). | Re-enters the Cook spine from the durable recipe. |
| `Regate` | The candidate is ready to re-verify. | Re-runs the recorded gates on the existing candidate, then publishes. |
| `Rearm` | The terminal continuation failed. | Rearms that continuation. |
| `Retry` | Retry admission accepts a fresh attempt. | Reserves and runs a successor attempt. |
| `ForkReplacement` | The checkpoint no longer matches its destination. | Forks a replacement attempt that keeps the evidence. |
| `Reconcile` | A promotion claim is still held. | Reconciles the claim first. |
| `ReplaceGates` | The applied candidate failed its gates and nothing else is admitted. | Needs explicit operator authorization: corrected gates or external proof. |
| `Publish` | A verified candidate is unpublished. | Publishes it from the durable record (as a draft without a usable review form). |
| `Adopt` | `no_changes`, but the destination branch has commits ahead of base. | Adopts that revision as the candidate. |
| `Blocked` | The previous `recover` performed this same step from this same state with no change. | Nothing; names the blocker so recovery cannot loop. |
| `None` | Nothing is legal (in flight, live driver, settled, incomplete review form, ...). | Nothing; `reason` says what is required. |

## Partial publication

For a green candidate whose agent declared `completion: incomplete`, run:

```sh
homeboy agent-task recover <attempt> --publish-partial
```

This keeps the recorded agent form unchanged. The PR is a partial-delivery draft,
quotes `incomplete_reason`, and links a follow-up issue on the source task's GitHub
tracker. Both tracker issues receive the publication receipt and backlinks.
Status and batch summaries use the existing `succeeded_partial` contract and
retain the undelivered remaining scope. The source issue is referenced, not closed.

The command refuses candidates without passing gates, an incomplete agent form,
or a GitHub issue source. `--preflight` validates these requirements without
creating tracker entries or publishing. A durable follow-up receipt and a remote
marker allow interrupted recovery to reuse the existing follow-up.

## Inputs

`CookRecoveryInputs` holds every fact the decision reads: the Cook status,
whether a lifecycle record exists, whether a driver is live, held claims,
retry and continuation admission, the recorded review form's incomplete
reason, an adoptable candidate ref, and the no-progress record. If a new
decision needs a new fact, add it to the inputs and read it in the decider.
Don't re-read it in a caller.

## Order matters

`decide_cook_recovery_step` checks in a fixed order: no lifecycle record, then
live driver, disconnected runner, in-flight status, held claim, agent-declared
incomplete, publishable candidate, regate, checkpoint mismatch, adoption, and
finally retry or continuation by status. Earlier checks protect later ones;
for example, a live driver owns every transition, so nothing after it may race
that driver.

## No-progress guard

`recover` records the step it performed and the durable state it started
from. If the next `recover` would perform the same step from an unchanged
state, `cook_recovery_step` returns `Blocked` instead. Any real progress clears
the record.

## Verifying a change

The decider's unit tests (`cook_recovery.rs`, `mod tests`) build
`CookRecoveryInputs` directly, one row per case. For a change that could shift
real outcomes, compare `recover --preflight` against status `Next:` across a
sample of real Cooks: a mutating step must match a plain `recover <cook>` or
`adopt` action, and a `None` step must match no mutating action.
Cancellation is final for the attempt's outcome, but a cancelled attempt's Cook
may still hand its remaining phases to a continuation owner
(`ContinuationClaimed` moves `cancelled` to `continuing`, and a released
continuation settles back to the recorded outcome).
