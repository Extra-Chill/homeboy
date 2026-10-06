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

Isolated Linux source/build was created at `/home/chubes/Developer/homeboy-15493-integrated-20261005` from the merged candidate before `origin/main` advanced to `9802738dd`. Logs are in that source's `logs/` directory. Candidate Linux results from that earlier integrated revision:

- Terminal/reconcile: 86 passed.
- Action eligibility: 10 passed.
- Rooted daemon routing: 17 passed.
- Focused staging cancellation guard: 1 passed.
- Homeboy binary build: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy -p homeboy-agents -p homeboy-core -p homeboy-lab-runner --lib`: completed with existing warnings. `-D warnings` fails in unchanged `crates/homeboy-error/src/lib.rs:244` (`should_implement_trait` for `ErrorCode::from_str`).

The Linux `execution::tests::handoff` partition ran 39/41; two daemon-exec error-message assertions failed at `handoff.rs:2019` and `:2431`. They concern missing error-detail wording and an empty-envelope error phrase. They are recorded in `logs/reverse-broker-handoff.log`; no immutable baseline comparison was completed, so these are not classified as environmental or pre-existing.

## Incomplete publication gates

This report records the work completed and the verification still needed; it does not assert that the requested publication gate is green.

- The specific broker HTTP ownership regression matrix (accepted response lost, unavailable lookup, fenced-before-POST, cleanup failure, and acceptance/cancel race) was not added. Existing reverse-broker handoff tests passed locally but do not exercise those lease transitions over actual broker requests.
- The earlier native CLI proof log at `/var/folders/lr/c_cmmt7s0592m4njz99v5yb40000gn/T/opencode/homeboy-15493-native-proof.log` records a successful controlled run against an earlier candidate, but the private fixture source was deleted. It was not restored into repository tests or rerun against the current integrated candidate. A tests-only baseline failure for that native harness was therefore not established.
- The Linux integration source predates the second `origin/main` merge (`369987a3a`); required Linux gates must be repeated on the exact final tree.
- The broad `lab_staging_controller::tests` invocation was stopped after 20 minutes because numerous parallel tests remained blocked; only the focused cancellation guard passed. `runner_staging_store` and `runner_staging_operation` were not reached.
- No final build/clippy/test run was performed after `369987a3a`.

The worktree should be left clean after committing this report. These outstanding checks need completion before PR publication.
