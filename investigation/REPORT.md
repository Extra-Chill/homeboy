# Accumulated test failures on `main` — investigation report

Window examined: 2026-07-27T18:43Z → 2026-07-28T17:15Z. 81 CI runs, 161 test phases.

## 1. Method (so the arithmetic is checkable)

**Source of evidence.** Per-job logs from `/repos/Extra-Chill/homeboy/actions/jobs/{id}/logs`, for the job literally named `homeboy / Test`.

> **`gh run view <id> --log` is not usable for this.** It truncates individual job logs. On run 30380078587 the Test job was 15,531 lines via the run zip and **26,353 lines** via the job endpoint — the run zip silently dropped the entire baseline phase, including a second occurrence of the failure that made the run interesting. Any count taken from `gh run view --log` undercounts. `harvest.py` (first attempt) used it; `harvest2.py` does not.

**Phase split.** The Homeboy composite action runs the suite twice. The line

```
Checking out baseline ref <sha> for differential gating
```

separates phase 0 (candidate, the PR head) from phase 1 (baseline, the merge base — i.e. `main`). A run whose candidate phase is clean never emits that line and has no phase 1. 81 runs → 162 possible phases; run 30359254985 produced no candidate test output (gate said `inconclusive: current=unavailable`), giving **161 phases: 118 completed + 43 killed**.

**Failure detection.** `^<timestamp>Z test <name> \.\.\. FAILED$`, anchored to end of line. libtest emits that line when, and only when, a test *completes* with a failure. Anchoring matters: the JSON `stdout_tail` blobs contain the same substring mid-line and would otherwise double-count. Deduplication is per test-name per phase (a name seen twice in one phase keeps the worst outcome), then aggregated across phases.

**Kill detection.** `Homeboy command timed out after` in the phase, corroborated by `"exit_code": 124` and, in the observation artifact, `child_supervision.cancellation_reason == "timeout"`.

**Control greps.** `rg` is not installed here. Every "no matches" conclusion was checked against a control that must match — e.g. `grep -rn "fn main" crates/` → 40; `grep -c "\.\.\. ok$"` → 134 on the same file where `\.\.\. FAILED$` → 2.

Scripts: `harvest2.py` (fetch + phase-split), `inventory.py` (aggregate), `watch.py` (per-run timeline). Raw: `harvest_all.json`, `inventory.json`.

## 2. Inventory

`fail/obs` is per **phase**. "completed" = the phase ran to a real `test result:` summary; "killed" = the phase was SIGKILLed at the 1500 s budget.

| test | crate / file | fail/obs | completed | killed | first fail | last fail | status |
|---|---|---|---|---|---|---|---|
| `detached_cook_accepts_reverse_capacity_queue_and_worker_completes_once` | root `homeboy` pkg, `tests/reverse_cook_queue_acceptance.rs:46` | 59/105 | **59/62** | 0/43 | 07-28T04:34 | 07-28T13:34 | **live** (unobserved since 13:34) |
| `release_blocks_preparation_and_publication_on_a_native_windows_workspace_build` | root `homeboy` pkg, `tests/release_workflow_test.rs` | 46/46 | **46/46** | 0/0 | 07-27T18:43 | 07-28T06:17 | **fixed** — test removed by `b4e2cd1a2` |
| `agent_task_gate::baseline_tests::bounded_baseline_gate_reaps_background_descendants_before_reader_join` | `homeboy-agents`, `src/agent_task_gate.rs:829` | 29/29 | 0/0 | 29/29 | 07-28T12:23 | 07-28T17:15 | **live** |
| `agent_task_gate::tests::existing_gate_commands_are_automatic_toolchain_requirements` | `homeboy-agents`, `src/agent_task_gate.rs:1933` | 29/29 | 0/0 | 29/29 | 07-28T12:23 | 07-28T17:15 | **live** |
| `agent_task_lifecycle::activity_provider::tests::probe_by_id_resolves_one_record_without_scanning_or_writing` | `homeboy-agents`, `src/agent_task_lifecycle/activity_provider.rs:200` | 29/29 | 0/0 | 29/29 | 07-28T12:23 | 07-28T17:15 | **live** |
| `agent_task_lifecycle::activity_provider::tests::show_activity_resolves_an_agent_task_id_through_the_indexed_probe` | `homeboy-agents`, `src/agent_task_lifecycle/activity_provider.rs:280` | 28/28 | 0/0 | 28/28 | 07-28T12:23 | 07-28T17:15 | **live** |
| `agent_task_dispatch_plan::tests::dispatch_plan_preflight_fails_on_unresolved_runtime_dependency` | `homeboy-agents`, `src/agent_task_dispatch_plan/mod.rs:1784` | 28/33 | 0/0 | 28/33 | 07-28T12:23 | 07-28T17:15 | **live** — *not* fixed |
| `agent_task_finalization::tests::durable_finalization_uses_promoted_files_for_clean_committed_candidate` | `homeboy-agents`, `src/agent_task_finalization/tests.rs:1759` | 24/29 | 0/0 | 24/29 | 07-28T13:07 | 07-28T17:15 | **live** |
| `agent_task_gate::baseline_tests::bounded_baseline_gate_is_cancelled` | `homeboy-agents`, `src/agent_task_gate.rs:809` | 24/29 | 0/0 | 24/29 | 07-28T12:23 | 07-28T17:15 | **live** |
| `read_only_cli_commands_complete_while_runtime_promotion_is_held` | root `homeboy` pkg, `tests/readonly_cli_lock_test.rs:10` | 10/161 | 10/118 | 0/43 | 07-28T15:29 | 07-28T17:15 | **fixed** — `2d7fa2c1b` (#10628) at 17:37Z, closed #10621 |
| `agent_task_finalization::tests::production_validator_accepts_only_the_exact_promoted_recovery_commit` | `homeboy-agents` | 6/29 | 0/0 | 6/29 | 07-28T12:23 | 07-28T14:55 | flaky |
| `agent_task_dispatch_plan::tests::dispatch_plan_preserves_abstract_executor_requirements` | `homeboy-agents` | 5/33 | 0/0 | 5/33 | 07-28T13:17 | 07-28T17:15 | flaky |
| `agent_task_lifecycle::tests::handoff_and_proxy::controller_proxy_is_queued_before_handoff_then_binds_runner_child` | `homeboy-agents` | 3/6 | 0/0 | 3/6 | 07-28T13:50 | 07-28T14:55 | flaky |
| `agent_task_lifecycle::tests::handoff_and_proxy::preacceptance_snapshot_binds_planned_runner_job_before_validation` | `homeboy-agents` | 3/6 | 0/0 | 3/6 | 07-28T13:50 | 07-28T14:55 | flaky |
| `hermetic_fixture_command_cannot_use_operator_paths_or_installed_homeboy` | root `homeboy` pkg, `tests/stale_linked_rig_recovery.rs:27` | 3/46 | 3/3 | 0/43 | 07-28T03:20 | 07-28T03:50 | fixed |
| `agent_task_dispatch_plan::tests::dispatch_plan_accepts_component_contracts_from_provider_config` | `homeboy-agents` | 2/33 | 0/0 | 2/33 | 07-28T12:23 | 07-28T13:25 | flaky |
| `agent_task_executor_evidence::tests::` ×4 (`links_…`, `persisted_input_redacts_…`, `re_linking_…`, `repeated_child_runs_…`) | `homeboy-agents` | 1/30 each | 0/0 | 1/30 | 07-28T15:42 | 07-28T15:42 | one-off (run 30374669252 only) |
| `agent_task_finalization::tests::production_validator_normalizes_changed_file_order_and_duplicates` | `homeboy-agents` | 1/29 | 0/0 | 1/29 | 07-28T14:55 | 07-28T14:55 | one-off |
| `agent_task_gate::tests::toolchain_preflight_preserves_only_declared_homes_for_cargo_on_path` | `homeboy-agents` | 1/29 | 0/0 | 1/29 | 07-28T14:55 | 07-28T14:55 | one-off |
| `agent_task_gate::tests::toolchain_preflight_reports_generic_initialization_failures_without_code_feedback` | `homeboy-agents` | 1/29 | 0/0 | 1/29 | 07-28T14:55 | 07-28T14:55 | one-off |

23 distinct names ever printed `FAILED`, out of 528 distinct names observed.

## 3. Flaky vs durably broken

**Durable (≥ 80 % failure whenever executed) — 9:**
`existing_gate_commands_are_automatic_toolchain_requirements` (100 %), `bounded_baseline_gate_reaps_background_descendants_before_reader_join` (100 %), `probe_by_id_resolves_one_record_without_scanning_or_writing` (100 %), `show_activity_resolves_an_agent_task_id_through_the_indexed_probe` (100 %), `release_blocks_preparation_and_publication_on_a_native_windows_workspace_build` (100 %, since removed), `detached_cook_…` (95 % of completed phases), `dispatch_plan_preflight_fails_on_unresolved_runtime_dependency` (85 %), `bounded_baseline_gate_is_cancelled` (83 %), `durable_finalization_uses_promoted_files_for_clean_committed_candidate` (83 %).

**Flaky (< 25 %) — 12:** everything else in the table, including all four `agent_task_executor_evidence` tests, which failed in exactly one phase of one run (30374669252) and passed in 29 other phases.

`read_only_cli_commands_…` is a third category: 0/151 phases before 07-28T15:29, then 10/10 after — a clean regression, not flakiness, now fixed.

## 4. The masking mechanism — and the correction to the framing

**`baseline_red` is not what is hiding these.** The deployed gate has a quieter branch.

`Extra-Chill/homeboy-action`, tag `v2` = `66fb512` (v2.8.25); `.github/workflows/ci.yml:148` pins `ref: v2`.

```
scripts/core/apply-differential-gate.py:207   if current <= base:
scripts/core/apply-differential-gate.py:208       adjusted[command] = "pass"
scripts/core/apply-differential-gate.py:210       ::notice::Differential gate accepted {command}: current={current} base={base}
```

`enforce-final-status.sh` errors on `fail` (`:54`) and `timeout` (`:58`), warns on `baseline_red` (`:62`) and `inconclusive` (`:65`), and has **no branch for a `pass` synthesised from two failures**.

Proof, run 30380078587 job 90345508296 (conclusion **success**, both phases completed, no timeout):

| line | output |
|---|---|
| 15524 | candidate: `test read_only_cli_commands_complete_while_runtime_promotion_is_held ... FAILED` |
| 15540 | candidate: `test result: FAILED. 0 passed; 1 failed; …` |
| 25950 | baseline: same test `... FAILED` |
| 25966 | baseline: `test result: FAILED. 0 passed; 1 failed; …` |
| 26154 | `##[notice]Differential gate accepted review test: current=1 base=1` |

`baseline_red` *does* also occur (`apply-differential-gate.py:192`), 12 times in this window, always with the message `baseline command … exited 124 before comparable counts were available` — i.e. it fires because the baseline **timed out**, not because the baseline had known failures. So in this window the two statuses have disjoint causes:

- `pass` (from `current == base`) hides *real, counted, reproducible* failures. **This is the leak.**
- `baseline_red` fires only for killed baselines and is the honest, visible signal.

**homeboy-action#305** admits `timeout` into the gate and guards the count comparison so a killed candidate can never be promoted to `pass`. It is merged (`23392ca`) and released as v2.8.26/v2.8.27 on `main`, but **`v2` still points at `66fb512` (v2.8.25)** — verified with `git merge-base --is-ancestor 23392ca v2` → false. So none of #305 is deployed to `Extra-Chill/homeboy` yet. #305 also does **not** touch the `current <= base → pass` branch (still `apply-differential-gate.py:235-236` on `main`), so re-pointing `v2` would not fix this leak.

## 5. The timeout confound — the correctness check

This is where the framing needed the most repair.

**The suite has been running in two completely different scopes.**

| window | test binaries | tests | outcome |
|---|---|---|---|
| 07-27T18:43Z → 07-28T10:35Z | 17–25 | 77–125 | always completed |
| 07-28T12:23Z → 17:15Z | 27–29 | 280–503 | killed at 1500 s in 43 of 46 phases |

`homeboy-agents` was in **none** of the small-scope runs. So all 17 `agent_task_*` names in the inventory were observed **only inside killed phases** — 0 completed observations each. The scope widened after #10449 ("changed-scope test gate is blind to Cargo workspace member crates") and the widened suite immediately blew the budget.

**What a kill destroys, and what it does not.** From run 30382101662's observation artifact:

- `test_failures.json` → `[]`
- `summary` → `{"exit_code":124,"failed":0,"passed":0,"skipped":0,"total":0}`
- `child_supervision` → `{"cancellation_reason":"timeout","exit_code":124,"timeout_ms":1500000,"elapsed_ms":1501188}`
- retained stdout → last 64 KB only

So the counts, the `failures:` blocks and the panic messages are all gone. **No root-cause evidence for any `agent_task_*` failure exists anywhere in CI.** The `... FAILED` lines survive because libtest prints them as each test completes.

**Are those failures real, or artefacts of a saturated runner?** Three arguments that they are real:

1. libtest prints `... FAILED` only on a *completed* failing test; a SIGKILL yields no line. Each line is a genuine failure. The kill causes an **undercount**, never a false positive.
2. Neighbours in the same binary at the same moment passed. From the retained `child_supervision.stdout_tail`, `matching_baseline_failure_is_distinct_from_a_new_failure ... ok` and ~25 other `agent_task_gate::*` tests are interleaved with the three FAILEDs. Resource starvation does not pick the same four tests out of ~30 every time.
3. Four of them are 29/29 and 28/28 across independent runners. That is not flakiness.

The three at 83–85 % are less certain and may be genuinely load-sensitive.

**And one case where load inverts the outcome.** `detached_cook_…` FAILED in 59/62 *completed* phases and passed in 43/43 *killed* phases — including within the same run (30362979347: candidate completed → FAILED, baseline killed → ok). When the suite is large it takes 431.81 s and passes; when it is small it panics on a 10 s deadline at `reverse_cook_queue_acceptance.rs:371` with `controller did not project terminal broker result`. So "killed run" is not a synonym for "worse"; it is a different environment, and both directions of confound are real.

## 6. Issues filed

| # | scope |
|---|---|
| [#10657](https://github.com/Extra-Chill/homeboy/issues/10657) | Differential gate rewrites an equal-failure candidate to `pass` (the masking mechanism) |
| [#10658](https://github.com/Extra-Chill/homeboy/issues/10658) | Seven `homeboy-agents` lib tests red on `main`, grouped |
| [#10659](https://github.com/Extra-Chill/homeboy/issues/10659) | `detached_cook_accepts_reverse_capacity_queue_and_worker_completes_once` |

**Grouping justification for #10658.** The seven need seven different fixes, but no root-cause evidence survives for any of them, so seven issues would each say "unknown panic". They share one systemic cause (merged unrun behind #10449, then unreportable behind #10639) and one unblocking action: run `cargo test -p homeboy-agents` once with a budget that lets it finish. Split after the panics exist.

Not filed — already covered: #10639/#10644 (1500 s budget; `main` now sets 2700 s via job `env:`), #10655 (`detached_cook` costing 431 s), #10632 (gate issues hide root identity), #10449 (scope blindness), #10621 (closed by the `read_only` fix).

## 7. Policy recommendation

Three changes, cheapest first.

**(a) Stop manufacturing `pass` from two failures.** `current <= base` should split:

- `current < base`, or `base == 0` → `pass`
- `current == base` **and** `base > 0` → `baseline_red`
- else → `fail`

This is a five-line change in `apply-differential-gate.py` and it costs nothing: `baseline_red` still only warns (`enforce-final-status.sh:62`), so no PR gets blocked. It just stops the run from claiming everything is fine. **This alone would have surfaced `release_blocks_…` on 07-27 instead of letting it sit red for 12 hours across 23 green runs.**

**(b) Make `baseline_red` accumulate somewhere durable, not just warn.** A warning on a PR that merges anyway is written to a page nobody revisits. Homeboy already has the right primitive: `homeboy issues` reconciles findings against the tracker, and it already opened #10621 automatically for a test failure. Wire the *baseline* phase into it: when a command comes back `baseline_red`, reconcile the baseline's failing test identities as an issue against `main`, keyed by the stable per-test fingerprint that `findings.json` already carries (`test::::<name>::test_failure`). Reconciliation means one issue per identity, reopened if it recurs, closed when it stops — which is exactly the ledger that is missing. Pair with #10632 so the title names the test.

**(c) Assert that the gate is measuring something.** The through-line across today's defects — the post-merge audit gate scanning zero files and passing, the release pipeline reporting `"Unknown error"`, the Test gate unable to distinguish slow from broken, and this — is that **every one of them reports success while asserting nothing**. The generic countermeasure is a liveness assertion on the measurement itself, not on its verdict:

- a `test` result with `total == 0` is never `pass` — it is `inconclusive` at minimum (#305 does this for the timeout path; it should be unconditional)
- a differential comparison where either side has no structured counts is never `pass`
- an audit/lint run whose scanned-file count is 0 for a non-empty changed set is a hard error

Concretely: add a `measurement_ok` predicate that every gate must satisfy before its verdict is read, and fail closed when it does not hold. That is the invariant all four defects violated.

## 8. What in the framing was wrong

- **`baseline_red` is not what is hiding these failures.** The masking status is `pass`, produced by `current <= base` at `apply-differential-gate.py:207-208`. `baseline_red` at least emits `::warning::` and renders :warning:; `pass` emits `::notice::` and renders :white_check_mark:. In this 81-run window `baseline_red` fired only for *killed baselines* — it was the honest signal, not the leak. Re-pointing `v2` at homeboy-action#305 would not close this.
- **Run 30374669252's ten `... FAILED` lines were not hidden behind anything.** That run **concluded `failure`**, correctly. Its candidate phase was killed at 1500 s (`exit 124`), which the deployed gate classifies as `timeout` and never admits to differential adjustment. #305 exists precisely to stop those from blocking. So the flagship example is a *false red*, not a hidden red.
- **The supplied list of ten was one short and one wrong.** It omitted `show_activity_resolves_an_agent_task_id_through_the_indexed_probe`, which failed in the same phase. And `dispatch_plan_preflight_fails_on_unresolved_runtime_dependency` was reported by another cook as fixed on `main`; it is not — it failed again at 17:15Z in run 30382101662, and 28 times out of 33 across the window.
- **The four `agent_task_executor_evidence` failures are noise.** They appear in exactly one phase of one run (30374669252) and pass in 29 others. Treating them as part of a standing ten-test cluster overstates the problem.
- **The list conflates two populations that need different treatment.** The `agent_task_*` names have zero completed-run observations; the genuinely masked-in-completed-runs failures are `detached_cook_…`, `release_blocks_…` and `read_only_cli_…`, none of which were in the supplied list.
- **`gh run view <id> --log` cannot be used to count this.** It truncates job logs and would have dropped the single most important piece of evidence.

What *was* right: main has been accumulating red tests that no gate surfaces, the timeout confound is real and load-bearing, and the earlier cook's ten-test list did need verifying.
