# Issue 17520 gate-verdict validation

## Branch

Merged `origin/main` at `eb916afedb0929b094be9ff33bb8625499c9b365`
(merge commit `74f8352afe`).

## Public Cook-path proof

`public_cook_gate_retry_records_executed_and_reused_verdicts` uses public
`verify_replacement_gates`, a real Git candidate, real shell gates, the durable
lifecycle store, and the production verdict executor. No verdict-store mocks.
The fixture seeds the prior failed Cook promotion; it does not dispatch a provider.

Gate A sleeps and records its execution. Gate B records candidate executions
and requires an external prerequisite. The immutable candidate tree, gate commands,
and declared environment remain identical between verification rounds. B's
prerequisite is restored before round two. The verified base passes B, so its
candidate failure cannot be accepted as inherited.

Round one executes A and B. Cook refuses finalization with `green_gate_status`
for B. Round two reuses A and executes B successfully, producing an applied
promotion. Assertions read the original durable A receipt and failed B receipt,
check the reused receipt's original-run link and saved timing, and count shell
execution logs: A = 1, B = 2.

Observed output from the focused test:

```text
Cook gate retry: first A=158ms B=58ms; retry A=reused saved=158ms B=58ms
1 passed; 0 failed
```

Measured avoided A execution: **158 ms**, from its recorded elapsed time and
matching reused `saved_ms` (not a wall-clock speedup estimate).

## Invalidation and promotion safety

`retry_executes_failed_gate_but_reuses_agent_pass` additionally runs real shell
commands through the shared production executor. After a candidate-tree change,
both A and B execute again (execution counts A = 2, B = 3). Changing A's command
then changing its declared environment each executes A again (count = 4).
`exact_tree_command_and_environment_reuse_only_passes` checks independent exact-key
misses and rejection of failed receipts. Cook's replacement-proof path also
requires the immutable admitted candidate and finalization-eligible green evidence.

The content-addressed key includes the full candidate tree, execution argv,
environment and policy. Unidentified external closures conservatively miss;
commands that mutate their candidate cannot publish a reusable pass.

## Acceptance commands

- `cargo test -p homeboy-agents --lib public_cook_gate_retry -- --nocapture`: passed.
- `cargo test -p homeboy-agents --lib gate`: 416 passed, 0 failed, 1 ignored.
- `cargo nextest run --no-fail-fast -p homeboy-agents --lib -E 'test(/gate/)'`:
  416 passed, 0 failed.
- `cargo fmt --check`: clean.

The original test failure was invalid reviewer-facing absolute paths in gate
commands. Paths now enter through declared environment variables; reviewer safety
was not relaxed. The test also respects Cook's refusal to import a failed
replacement proof, reading durable execution receipts rather than expecting a
successful API response for that failure.
