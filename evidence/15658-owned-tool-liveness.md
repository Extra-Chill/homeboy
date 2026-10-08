# Owned tool activity and provider liveness

Tracker: https://github.com/Extra-Chill/homeboy/issues/15658

## Cause and boundary

The observed Homeboy 0.416.0 attempt was waiting on a quiet tool subprocess
writing measurement evidence outside the provider workspace. The liveness
watchdog only sampled provider pipes, runtime artifacts and workspace edits.
It terminated the attempt at its 300-second liveness window despite ongoing
subprocess work. The attempt was
`agent-task-10719ab6-b4d5-453e-a422-4058dc29cf63-attempt-1-2626a353`.

The repair samples Linux tool descendants through the existing execution owner.
Advancing CPU ticks or I/O character counters for a previously observed PID and
kernel start-time identity refresh the monotonic liveness clock. New identities,
unchanged counters, unreadable counters and process existence alone do not.
The provider root is excluded, so provider housekeeping cannot mask an idle tool.
Samples use the existing bounded workspace-sampling cadence. Absolute execution
deadlines, bounded wall-clock extensions and owned-tree teardown still apply.

This is activity evidence, not proof of useful semantic progress. An active
infinite loop is bounded by the wall-clock cap. Linux ancestry sampling does not
observe work delegated to unrelated container daemons or already-reparented
processes. Non-Linux platforms retain their existing liveness signals.

## Deterministic verification

Baseline: `8d0294a23` (origin/main, v0.417.4).
Linux verification used an isolated source copy and target directory.

```sh
cargo test -p homeboy-agents --lib quiet_ -- --nocapture --test-threads=1
cargo test -p homeboy-agents --lib agent_task_provider::tests::scheduler_tests -- --nocapture --test-threads=1
cargo test -p homeboy-engine-primitives --lib command::tests -- --test-threads=1
```

The initial test-only baseline run failed both CPU and external-file I/O
descendant completion tests with `agent_task.provider_liveness_timeout` at
1000ms. The idle descendant control passed. The candidate's provider suite
passed 67 tests, including those regressions and an active infinite child
bounded by an absolute execution deadline. The process-owner lifecycle suite
passed 20 tests, including cancellation, controller loss and cleanup.
The final primitive suite passed 284 tests (three subprocess fixtures ignored),
and all four final quiet-tool regressions passed. Formatting and diff checks pass.
Strict Clippy exposes existing warnings in unchanged code; the change's own
diagnostic was corrected. Non-strict scoped Clippy remains blocked by the
existing `clippy::never_loop` error in `agent_task_service/cook.rs:4398`
(unchanged from the baseline). No lint suppression or unrelated repair was added.

The real-process fixtures compress the production liveness window and use
neutral Node children, with no website or provider-service dependencies.

## Workflow and AI assistance

Chris Huber authorized direct OpenCode recovery and requested this Homeboy PR.
Implementation and finalization occur outside Cook in an isolated tracker-linked
Git worktree. OpenAI `openai/gpt-6.1-sol` through OpenCode investigated, implemented
and verified the repair. Session: `ses_ee3f5aef6ffer5InTsIcx39n99`.
This repair has not been deployed or used to rerun the original import task.
