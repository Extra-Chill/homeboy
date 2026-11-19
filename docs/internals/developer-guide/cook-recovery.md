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

`agent_task_lifecycle/cook_run_state.rs` owns Cook state writes. `CookRunState::of(record)`
prefers the internal persisted state on the authoritative lifecycle record;
only records without it use the legacy classifier. Remove that fallback after
all retained records have been rewritten by a state-aware binary.

The lifecycle store adapts terminalization, harvest, regate/recover, cancellation,
requeue and continuation metadata writes at their shared revision-fenced commit
boundary. Phase transitions are validated against the stored state, and the new
phase, terminal/retryable/recoverable facts and replay receipts commit atomically
with the lifecycle changes. `apply(event, key)` rejects illegal edges and key
reuse for different events; replaying an event/key is a no-op even after later
events. Store adapters use the next run revision as the per-run key. Identical
facts do not create another receipt. Unfenced test fixtures seed rather than
transition state.

The internal store encoding includes state and receipts; ordinary record JSON
omits them, preserving command output contracts. The record's private field
requires an authority token only the state module can create. Subsystem event
adapters and unified liveness remain subsequent slices: ownership/fence facts
are captured at commit time rather than probed again by `of`.

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
