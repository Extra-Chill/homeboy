# Admission recovery — #15605

Tracker: https://github.com/Extra-Chill/homeboy/issues/15605

## Root cause and consolidation

A controller-cancelled Cook still had an unstarted queued handoff on a retained
daemon generation. The runner's typed job view correctly counted it as work,
while the current controller had already recorded an exact terminal cancellation.
Generation reconciliation only asked the old daemon to recover its own terminal
records; it never delivered that controller cancellation. The retained generation
therefore fenced refresh indefinitely.

Use the existing terminal-recovery provider for canonical controller cancellation
truth, and the existing authenticated generation transport for exact job
cancellation. Re-observe the daemon after that request. The same lease/PID,
current-work, ambiguity and evidence-retirement guards still govern rotation.
Only queued, exactly linked cancelled handoffs are eligible. Running jobs,
unresolved records and other terminal outcomes remain untouched. Idle generations
do not enumerate historical job payloads.

Cook preview and execution now use the same placement admission predicate even
when preview's read-only resource classification is `NotRequired`. Stale or
disconnected Lab evidence cannot silently authorize controller execution.

## Regression and runtime evidence

- Lab run `hb-15605-regressions-20261006`: exact cancellation controls and
  persisted-record terminal recovery passed.
- Lab run `hb-15605-http-preview-passed-20261006`: real HTTP cancellation and
  re-observation, plus the read-only placement regression, passed.
- Lab run `hb-15605-broader-gates-20261006`: all 52 generation-store tests passed,
  including process-isolated custody, concurrency and retained-evidence restart
  proof. Initial snapshot runs exposed build-provenance scoping in nested fixture
  builds; those runs were superseded by the clean provenance-bound gate below.
- Lab run `hb-15605-provenance-scoped-gates-20261006`: all 187 selected tests
  passed (52 generation-store, 129 refresh, one terminal-recovery, one placement
  matrix and four preview projection tests), plus formatting. The nine ignored
  helper processes are driven by their enclosing process-isolated tests.
- Live candidate `target/debug/homeboy runner reconcile <runner>` delivered the
  already-recorded cancellation to its exact retained endpoint. A direct remote
  job read confirmed `cancelled`; that generation changed from one active job to
  zero and admission's `safe_to_rotate` changed from false to true. Retained run
  and artifact custody was preserved.
- Live candidate Cook preview reproduced the original command and reported
  `placement.admission.state=blocked` with an explicit local replay command.
- The documented pinned refresh then passed the old diagnostic-SSH refusal. It
  encountered a separate active runtime-promotion pin; that owner was preserved.

Reproduction commands:

```sh
cargo test -p homeboy-lab-runner generation_store -- --test-threads=1
cargo test -p homeboy-lab-runner homeboy_refresh -- --test-threads=1
cargo test -p homeboy-agents api_jobs_terminal_recovery -- --test-threads=1
cargo test -p homeboy-cli cook_admission_placement_is_independent -- --test-threads=1
cargo test -p homeboy-cli commands::agent_task::run::tests::preview -- --test-threads=1
cargo fmt --all -- --check
```

For a sealed source snapshot without Git metadata, pass its actual committed
source identity through the supported `HOMEBOY_PRODUCT_GIT_COMMIT` and
`HOMEBOY_PRODUCT_GIT_DIRTY` build inputs. Do not stamp a dirty snapshot as clean.
Scope these inputs to compilation, then run Cargo's reported test executables
without those environment overrides: nested fixture builds must derive their own
Git identity, not inherit the parent candidate's commit.

## Execution provenance

Direct OpenCode implementation was explicitly authorized. Work remained in an
isolated tracker-linked worktree; Rust regression verification ran on Lab through
diagnostic SSH while normal admission was blocked. A separate native controller
candidate supplied the live recovery proof. Coding finalization occurs outside
Homeboy Cook; no provider task or source-session message was submitted.

AI assistance: OpenAI gpt-6.1-sol via OpenCode investigated, implemented,
self-reviewed and verified this change. Session: `ses_eed67ca98ffemj4hPHlrqRTfLJ`.
