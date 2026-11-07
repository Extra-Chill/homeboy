# PR: Converge Lab attempts after provider exit

## Root cause

The controller's reconcile preview read its stale running record without consulting the retained runner authority. Provider supervision also stopped at provider exit without an explicit harvesting transition. The candidate uses the existing validated runner record projection for preview and apply, relays runner progress, and records provider completion, harvesting and gate materialization.

Removed synthetic post-provider harvesting heartbeats: they can overwrite a later phase; the scheduler already records the actual transition.

## Primary evidence

`cargo test -p homeboy-agents reconcile_observes_terminal_lab_attempt`: passes. The public reconcile/status regression supplies a terminal retained Lab run while the controller remains running, verifies preview does not persist, then verifies apply and status converge.

Additional validation:
- Lifecycle tests: 362 passed.
- Reconcile service tests: 24 passed.
- Lab execution tests: 150 passed.
- Agents library: 2563 passed, 2 ignored.
- Fixed detached cleanup test to recognize terminal `candidate_recoverable`; focused test passes.
- Current focused regression: `cargo test -p homeboy-agents reconcile_observes_terminal_lab_attempt_while_controller_is_running` — 1 passed.
- Final broader gate: `homeboy review test homeboy` — exit 0; format and clippy passed; 14146 tests passed, 27 skipped. Both previously reported failures, `cancelling_interrupted_pre_projection_launcher_leaves_no_admitted_orphan` and `execution_step_survives_daemon_process_group_sigkill`, passed within this full run (4.711s and 40.879s respectively).
- The supplied gate-failure excerpt contains test IDs, not a failure diagnostic. The prior failures did not reproduce in this workspace; no speculative production or test changes were made to compensate for them.

Fixes #17487

AI disclosure: OpenAI model exposed by the harness as `cook-heavy` (Roadie provider), using pi. AI read the issues, inspected the inherited implementation, removed compensating heartbeat behavior, corrected the cleanup test's terminal-state classification, and executed validation. The underlying model version is not exposed.
