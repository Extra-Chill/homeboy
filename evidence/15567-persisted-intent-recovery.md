# Cook canonical lineage and persisted-intent recovery

Trackers: #15567, #15562, #13755. Follow-up to merged #15577.

## Root cause and consolidation

The atomic recipe writer landed in #15577, but Cook still reconstructed absent
edges from lifecycle metadata and provider artifact provenance. Its follow-up
reuse path also required a lifecycle record: a crash after recipe append and
before submission could allocate a second intent for the same candidate.

This change makes persisted recipe lineage the Cook parent authority, removes
the reconstruction and legacy rebinding paths, and resumes the exact persisted
follow-up before allocating a run or reserving another category budget.
Replacement edges retain the original execution purpose through persisted
ancestry, including bounded provider-discovery recovery for review-only work.
Generic unbound lifecycle retry admission and provider artifact evidence retain
their distinct roles.

Recipe validation rejects duplicate run identities, dangling/forward edges, and
replacement edges that skip the latest attempt. Control-only pre-provider
corrections preserve an unchanged attempt binding instead of duplicating it.
Replay validates frozen request identity and admission scope both before and
after lifecycle submission. It runs under the existing per-Cook driver lock.

## Real-store regression

`persisted_follow_up_intent_recovers_before_submission_without_allocating_a_twin`
uses a real Git candidate patch, authenticated digest, persisted recipe, and
lifecycle store. The fixture appends a follow-up but leaves its lifecycle record
absent, reproducing the crash boundary.

- Changed instructions are refused with zero provider starts.
- Reopening the recipe store twice reuses the same run ID and exactly two
  recipe entries, with one provider execution.
- Exhausted replay budget does not cause another category reservation.
- Changed replay inputs after submission are also refused, preserving recipe
  identity and the single execution count.

Baseline `077438903b23401f1d26513e93b81cd11cb8e742` (#15577), with only the
regression fixture overlaid, fails: **three recipe attempts and one provider
start for the conflicting intent**. Nextest run:
`360d1bc7-a6e9-4e05-8516-6452b6b04b15` (0 passed, 1 failed, 2,759 skipped).

## Exact-candidate verification

Production/test source: `f547c011e4` on base `2bf0284d48`. Default stack;
`RUST_MIN_STACK` unset; eight build jobs. Temp files use a spacious filesystem
rather than the host's capacity-constrained tmpfs. Reserve gates remain intact.

```sh
env -u RUST_MIN_STACK CARGO_BUILD_JOBS=8 cargo nextest run \
  -p homeboy-agents -p homeboy-cli \
  -E 'test(cook) | test(execution) | test(promotion) | test(adoption) | test(baseline) | test(lifecycle_store)' \
  --no-fail-fast --status-level fail --final-status-level fail
```

**1,378 passed, 4,551 skipped**; Nextest
`8702e9a2-87ab-425e-8f0f-154c5c0be9cc`. This is scoped runtime/store/CLI
verification, not the full repository suite.

Adding `-p homeboy-lab-runner` ran 1,730 tests. First run
`616b3c04-d1e8-49b9-ab31-2b81e463a3a5`: 1,728 passed, 2 failed. Unchanged
candidate rerun `96c6e5cb-8075-4720-8687-d9f755808b62`: 1,729 passed, 1 failed.
The remaining failure, `existing_local_cwd_outside_the_root_recommends_sync_workspace`,
assumes temporary paths lie outside the configured workspace root. It also fails
on unchanged base `2bf0284d48`, run `80fc0f44-98a1-46c5-bbdf-8fab117d27af`.
The first run's provider-source handoff failure passed both the base comparison
and unchanged candidate rerun; its initial failure remains recorded here.

## Compatibility and custody

Missing lineage does not grant parent authority. Old metadata/provenance-only
attempts are not silently migrated or rebound. The recipe schema remains v1;
roots retain an absent edge. This intentionally retires the old read contract.

Finalization occurs outside Homeboy orchestration under the operator-authorized
simplification workflow. Evidence was produced through direct Lab verification.
AI contribution: OpenAI `openai/gpt-6.1-sol` through OpenCode, directed by Chris
Huber, implemented and reviewed the hardening and ran the recorded verification.
