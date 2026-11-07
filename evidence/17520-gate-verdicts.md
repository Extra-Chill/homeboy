# Issue 17520 validation (incomplete)

This Cook runs on `homeboy-lab`; local execution here is Lab execution. No
remote runner or `--placement lab` is needed. The previous remote-runner
preflight was not relevant and has been removed.

## Changes

- Public adapter plans are runnable through `agent-task gate`, using the same
  executor/store as shell gates.
- Promotion checks exact verdicts before candidate materialization/dependency
  hydration; partial retries exclude cached commands from dependency setup.
- Local execution checks before Rust cache/target/component setup.
- Providers carry the controller's durable verdict data root into isolated
  attempts; agent gate policy reads use that root too. Unknown external
  extension closures conservatively miss.

## Validation on homeboy-lab

`cargo build --release -p homeboy-cli` passed (optimized library package).
This package does not produce the CLI executable; additionally built
`cargo build --release --bin homeboy`, which passed in 9m 43s.

`cargo test -p homeboy-agents --lib agent_task_gate::verdict`:

```text
running 2 tests
test agent_task_gate::verdict::tests::exact_tree_command_and_environment_reuse_only_passes ... ok
test agent_task_gate::verdict::tests::retry_executes_failed_gate_but_reuses_agent_pass ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 2569 filtered out; finished in 1.70s
```

The tests exercise real shell execution, exact-key misses, failure reexecution,
original-result links, and pre-setup lookup matching the executed verdict.
They are not a substitute for the requested Cook proof.

## Real Cook attempt (not completed)

Temporary HOME: `/tmp/homeboy-17520.Pg6pVY/home`; throwaway Git repository:
`/tmp/homeboy-17520.Pg6pVY/repo`, linked destination `.../work`.
Used the built release binary, registered the component and local-shell runtime,
and started the isolated daemon with `daemon recover --yes`.

```text
homeboy agent-task cook --placement local \
  --cwd /tmp/homeboy-17520.Pg6pVY/work --repo repo \
  --backend local-shell --goal proof \
  --verify 'sleep 60; true' --verify false \
  --no-finalize --max-attempts 1 --run-id proof-17520-real
```

Actual `agent-task status proof-17520-real --exact` output:

```text
Agent task status
Status: failed
Run: proof-17520-real
Reason: no failure diagnostic was recorded for `failed` during `pre_supervisor`; inspect execution evidence
Artifacts: 0
Next: homeboy runs evidence proof-17520-real
```

`runs evidence proof-17520-real` returned `count: 0`, with
`durable_read.authoritative_aggregate_absent`. Raw command outputs remain under
`/tmp/homeboy-17520.Pg6pVY/` on this host.

No agent gate pass, promotion reuse, or failed-second-gate retry was observed in
this Cook. **Measured seconds saved: not established.** The required real-Cook
proof remains incomplete; do not report the test's projected savings as measured
Cook savings. The local-shell runtime used here is only a no-op executor; an
agent-capable executor must actually invoke the declared gate for the proof.
