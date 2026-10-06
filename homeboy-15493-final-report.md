# Homeboy #15493 — final publication verification

## Candidate and commits

- Checkout: `/Users/chubes/Developer/homeboy@direct-15493-virtual-care-20261005`
- Branch: `fix/15493-rooted-staging-cancel-direct`
- Base implementation: `1eb86f16d1a53badacf3e998e59311874d23be1d`
- Follow-up checkpoint: `d7930bf78` (`fix(lifecycle): fence pending runner cancellation`)
- Main integration commits: `a60ee7e0f` and `369987a3a` (ordinary merge commits; latter includes current `origin/main` at `9802738dd`)
- No push or GitHub publication was performed.

## Changes and verification

The retained five-file WIP was committed in `d7930bf78`. Pending runner admission now returns a typed distinction between rejection before transport and failure from the submission/resolve callback. The broker's outer cleanup releases the lease only for a rejected admission; ambiguous submission custody and proven-nonacceptance cleanup remain with the transport resolver. The pending-preparation test request fixture was corrected to use `RunnerApiSubmitRequest`.

Local verification passed:

- `cargo fmt --all -- --check`
- `cargo test -p homeboy-agents --lib terminal_and_reconcile` — 85 passed before latest-main integration.
- `cargo test -p homeboy-lab-runner --lib execution::tests::handoff` — 41 passed.
- `cargo test -p homeboy-agents --lib action_eligibility::tests` — 10 passed.
- `cargo test -p homeboy-core --lib daemon::generation_store::tests` — 17 passed.
- `cargo check -p homeboy-lab-runner`

Isolated Linux source/build is at `/home/chubes/Developer/homeboy-15493-integrated-20261005`; after the second merge it was refreshed from exact `HEAD` (`369987a3a`) and the relevant gates were rerun. Logs are in that source's `logs/` directory:

- Latest-main terminal/reconcile: 86 passed (`final-terminal-and-reconcile.log`).
- Latest-main action eligibility: 10 passed (`final-action-eligibility.log`).
- Latest-main rooted daemon routing: 17 passed (`final-rooted-daemon-routing.log`).
- Latest-main reverse-broker handoff subset: 3 passed (`final-reverse-broker-handoff.log`).
- Latest-main focused staging cancellation guard: 1 passed (`final-staging-cancel.log`).
- Latest-main `cargo build --bin homeboy` and `cargo fmt --all -- --check`: passed.
- `cargo clippy -p homeboy-agents -p homeboy-core -p homeboy-lab-runner --lib`: completed with warnings. `-D warnings` fails in unchanged `crates/homeboy-error/src/lib.rs:244` (`should_implement_trait` for `ErrorCode::from_str`).

The Linux `execution::tests::handoff` partition ran 39/41; two daemon-exec error-message assertions failed at `handoff.rs:2019` and `:2431`. They concern missing error-detail wording and an empty-envelope error phrase. They are recorded in `logs/reverse-broker-handoff.log`; no immutable baseline comparison was completed, so these are not classified as environmental or pre-existing.

## Incomplete publication gates

This report records the work completed and the verification still needed; it does not assert that the requested publication gate is green.

- The specific broker HTTP ownership regression matrix (accepted response lost, unavailable lookup, fenced-before-POST, cleanup failure, and acceptance/cancel race) was not added. Existing reverse-broker handoff tests passed locally but do not exercise those lease transitions over actual broker requests.
- The earlier native CLI proof log at `/var/folders/lr/c_cmmt7s0592m4njz99v5yb40000gn/T/opencode/homeboy-15493-native-proof.log` records a successful controlled run against an earlier candidate, but the private fixture source was deleted. It was not restored into repository tests or rerun against the current integrated candidate. A tests-only baseline failure for that native harness was therefore not established.
- The two Linux daemon-exec message assertion failures remain unclassified because an immutable baseline comparison was not completed.
- The broad `lab_staging_controller::tests` invocation was stopped after 20 minutes because numerous parallel tests remained blocked; only the focused cancellation guard passed. `runner_staging_store` and `runner_staging_operation` were not reached.
- The exact-current Linux reverse-broker coverage was limited to the three reverse-broker subset tests; the wider handoff partition still has the two recorded daemon-exec failures.

The worktree is clean after committing this report. The outstanding proof and regression tests need completion before PR publication.

## Resume verification — 2026-10-06

- Merged current `origin/main` `68ba10305` normally; merge commit `7cd2bbd6f` is the current candidate. Added `35e2f10ef` to align the CLI deferred-cancellation regression with the authoritative failed/nonterminal action acknowledgement (`exit_code=1`, outcome `failed`). The focused test passed locally and on Linux.
- Re-ran the two previously failing Linux daemon-exec assertions against immutable latest-main source and the exact merged candidate: both pass in both trees. They were resolved by current-main integration.
- Exact-head Linux additional gates passed: `runner_staging_store::tests` (12), `runner_staging_operation::tests` (22), `lab_staging_controller::tests::detached_staging` (2), detached reverse-broker handoff (1), rooted controller-staging blocker (1), rooted pending submission root isolation/fence (1), and pending cancellation effect/replay (1).
- Exact-head Linux `cargo test` for pending cancellation effect took 15.25 seconds and passed. Prior broader candidate checks and CI artifacts remain as recorded above.
- Still incomplete: actual broker HTTP owner-lease transition regression matrix, reproducible native private-daemon test source and exact-head rerun, and full exact-head Linux broad gates after the latest merge. The PR must remain draft. Do not treat previous native log output as exact-head proof.
- The prior CI failure on `cancel_command_reports_a_deferred_cancellation_without_claiming_the_run_is_cancelled` came from the new failed/nonterminal acknowledgement contract; the regression now asserts the correct failed outcome and nonzero exit. The refreshed CI run after publication will be authoritative.
