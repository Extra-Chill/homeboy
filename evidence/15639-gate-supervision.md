# Gate supervision is not candidate-code evidence

Base: `47165dce3c74cafa2e162eaa736736516df01076` (v0.417.9).

The subprocess supervisor retained `NoProgress` and `TimedOut`, but evidence
construction treated their synthetic 125/124 codes as command-owned failures.
The repair classifies these outcomes as `execution_budget`, names the applicable
limit and termination cause, and leaves captured tails available under existing
visibility policy. Completed commands returning 124/125 remain candidate failures.

Cook now gives typed budget termination precedence over persisted candidate-code
labels and baseline deltas. Neither candidate repair nor no-change repair is
dispatched. Feedback reports `gate_budget_exceeded`; the Cook spine uses the
existing `timed_out` lifecycle status and preserves unproven verification.

## Native macOS proof

Commands used these environment settings:

```
RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
```

- Unchanged base, with only regression assertions added:
  `cargo test -p homeboy-agents agent_task_gate::tests::silent_gate_stall_is_not_a_semantic_command_failure -- --exact --nocapture`
  failed semantically: actual `candidate_code`, expected `execution_budget`.
  The subprocess was actually killed by the 30 ms silence supervisor.
- Candidate: `cargo test -p homeboy-agents agent_task_gate::tests -- --test-threads=1`
  passed **88/88**, including no-progress/timeout cause and limit checks, direct
  command-owned 124/125 failures, private evidence and declared test outcomes.
- Candidate: `cargo test -p homeboy-agents agent_task_cook_loop::tests -- --test-threads=1`
  passed **36/36**. The added proof executes both real supervisor terminations,
  then exercises changed/unchanged candidates with historical misclassification
  and a candidate-regression comparison: no follow-up, two retries still available.

An initial two-thread gate suite hit process-global environment fixture interference;
isolated no-progress proof and the complete serial suites passed.

Strict native lint (`cargo clippy -p homeboy-agents --lib --no-deps -- -D warnings`)
failed with **75 diagnostics on both base and candidate**, at pre-existing sites.
This is not reported as a green lint run. Formatting and whitespace checks pass.

## Lab attempt

Runner job `69701f5b-5bb2-4877-a6ab-c217b25ec347`, run
`gate-15639-baseline`, attempted the unchanged-base subprocess regression with
exclusive target `/tmp/homeboy-target-15639`. Compilation failed writing
`homeboy-core` metadata with `Disk quota exceeded (os error 122)`; no test ran.
The native differential proof above supplies the actual regression evidence.
