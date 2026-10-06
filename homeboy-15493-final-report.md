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

## Incomplete publication gates at initial checkpoint (superseded)

This report records the work completed and the verification still needed; it does not assert that the requested publication gate is green.

- The specific broker HTTP ownership regression matrix (accepted response lost, unavailable lookup, fenced-before-POST, cleanup failure, and acceptance/cancel race) was not added. Existing reverse-broker handoff tests passed locally but do not exercise those lease transitions over actual broker requests.
- The earlier native CLI proof log at `/var/folders/lr/c_cmmt7s0592m4njz99v5yb40000gn/T/opencode/homeboy-15493-native-proof.log` records a successful controlled run against an earlier candidate, but the private fixture source was deleted. It was not restored into repository tests or rerun against the current integrated candidate. A tests-only baseline failure for that native harness was therefore not established.
- The two Linux daemon-exec message assertion failures remain unclassified because an immutable baseline comparison was not completed.
- The broad `lab_staging_controller::tests` invocation was stopped after 20 minutes because numerous parallel tests remained blocked; only the focused cancellation guard passed. `runner_staging_store` and `runner_staging_operation` were not reached.
- The exact-current Linux reverse-broker coverage was limited to the three reverse-broker subset tests; the wider handoff partition still has the two recorded daemon-exec failures.

At this initial checkpoint, the worktree/report were not a completed publication gate. The follow-up section below supersedes the initial missing-test statements.

## Resume verification — 2026-10-06

- Merged current `origin/main` `68ba10305` normally; merge commit `7cd2bbd6f` is the current candidate. Added `35e2f10ef` to align the CLI deferred-cancellation regression with the authoritative failed/nonterminal action acknowledgement (`exit_code=1`, outcome `failed`). The focused test passed locally and on Linux.
- Re-ran the two previously failing Linux daemon-exec assertions against immutable latest-main source and the exact merged candidate: both pass in both trees. They were resolved by current-main integration.
- Exact-head Linux additional gates passed: `runner_staging_store::tests` (12), `runner_staging_operation::tests` (22), `lab_staging_controller::tests::detached_staging` (2), detached reverse-broker handoff (1), rooted controller-staging blocker (1), rooted pending submission root isolation/fence (1), and pending cancellation effect/replay (1).
- Exact-head Linux `cargo test` for pending cancellation effect took 15.25 seconds and passed. Prior broader candidate checks and CI artifacts remain as recorded above.
- Still incomplete: actual broker HTTP owner-lease transition regression matrix, reproducible native private-daemon test source and exact-head rerun, and full exact-head Linux broad gates after the latest merge. The PR must remain draft. Do not treat previous native log output as exact-head proof.
- The prior CI failure on `cancel_command_reports_a_deferred_cancellation_without_claiming_the_run_is_cancelled` came from the new failed/nonterminal acknowledgement contract; the regression now asserts the correct failed outcome and nonzero exit. The refreshed CI run after publication will be authoritative.

## Acceptance gaps closed — 2026-10-06

- Fixed the supervisor-reviewed preparing→pending race: exact and resolved cancellation now acquire the selected lifecycle store's handoff lock before re-reading/classifying the intent, and use that locked state for acceptance lookup, fencing and terminal decisions.
- Added `cancellation_reloads_preparing_to_pending_intent_after_waiting_for_handoff_lock`. It holds the handoff lock while the intent advances to pending and the real in-memory broker `JobStore` accepts the request; after release, cancellation binds and cancels that exact job before terminalizing. The same-ID independent-root test remains in the Linux terminal/reconcile partition.
- Extracted the production broker HTTP ownership path into the broker admission/resolution helpers and added five real TCP/HTTP regressions: accepted response lost, unavailable lookup, pre-POST cancellation fence, proven-absence cleanup failure, and acceptance/cancel race. They inspect actual request paths/bodies and owner lease transitions; the race uses durable lifecycle state and a real `JobStore`, and confirms no false terminalization and no later POST.
- Restored `crates/homeboy-agents/tests/native_15493_private_daemon.rs`. On the exact integrated Linux candidate, it launched a private Homeboy daemon, admitted a blocking controller-owned staging job, invoked the real CLI, confirmed repeated failed/requested/nonterminal acknowledgement while owner remained running, checked same-run-ID root isolation, released the owned child, and verified terminal cancellation plus zero later runner submissions.
- Exact Linux proof command (run with the isolated integrated source and shared Cargo target): `HOMEBOY_NATIVE_BINARY=/home/chubes/Developer/_homeboy_cargo_shared_target/debug/homeboy CARGO_TARGET_DIR=/home/chubes/Developer/_homeboy_cargo_shared_target CARGO_BUILD_JOBS=2 cargo test -p homeboy-agents --test native_15493_private_daemon -- --test-threads=1 --nocapture`; result: 1 passed. Output: `/home/chubes/Developer/homeboy-15493-integrated-20261005/logs/native-proof-current.log`.
- Exact Linux focused gates passed: terminal/reconcile 87; cancellation 12; action eligibility 10; daemon generation store 17; runner staging store 12; runner staging operation 22; detached staging controller 2; reverse-broker handoff 41; broker ownership HTTP matrix 5. Exact output logs are in `/home/chubes/Developer/homeboy-15493-integrated-20261005/logs/final-*-current.log`.
- Exact Linux `cargo fmt --all -- --check`, `cargo build --bin homeboy`, and scoped `cargo clippy -p homeboy-agents -p homeboy-core -p homeboy-lab-runner --lib` passed; clippy emitted warnings. No gates were skipped or weakened.
- Final exact implementation candidate: `a6d67692f516c1ea7674da5a6256394d2e10886c`, ordinary merge of `origin/main` `15ee4f7a375db450334d34c8475fac968d676ee1` with scoped cancellation follow-up `0e6d79355` and the publication-evidence commit. Exact-head Linux native proof passed (1/1); all scoped Linux gates passed at this SHA. Logs: `/home/chubes/Developer/homeboy-15493-integrated-20261005/logs/native-proof-a6d67692f.log`, `terminal-a6d67692f.log`, `cancellation-a6d67692f.log`, `action-eligibility-a6d67692f.log`, `generation-store-a6d67692f.log`, `staging-store-a6d67692f.log`, `staging-operation-a6d67692f.log`, `detached-staging-a6d67692f.log`, `runner-handoff-a6d67692f.log`, `broker-ownership-a6d67692f.log`, `fmt-a6d67692f.log`, `build-a6d67692f.log`, and `clippy-a6d67692f.log`.
- Linux native proof invocation: `HOMEBOY_NATIVE_BINARY=/home/chubes/Developer/_homeboy_cargo_shared_target/debug/homeboy CARGO_TARGET_DIR=/home/chubes/Developer/_homeboy_cargo_shared_target CARGO_BUILD_JOBS=2 cargo test -p homeboy-agents --test native_15493_private_daemon -- --test-threads=1 --nocapture` (1 passed). It invokes the candidate CLI against a private daemon, keeps the real controller-owned staging job running during first cancel and idempotent replay, verifies failed/requested/terminal=false acknowledgement, then confirms terminal cancellation only after owner resolution; same-ID second-root stays queued and late runner POST admission remains zero.
- Final PR publication still requires authoritative CI on the pushed exact head and independent supervisor review. This task does not mark ready or merge.

## Independent supervisor replay — 2026-10-06

- Reproduced the actual installed-runtime discrepancy: the owned controller staging job was cancelled while its corresponding agent-task record remained queued and exact cancellation was still declared unsupported. The installed CLI was `0.406.4`; the candidate completes the missing rooted transport.
- Downloaded the immutable `homeboy-candidate-binary-1` artifact from CI run `37485063608` for head `276a0898b2ebcf5dd2b7e0de9393fce3c6a92dee`. Binary SHA-256: `a59cb409038a5887b0880d08cf1b5a680b2dc1809b07db633ff8011d23aea646`.
- The initial independent native replay stalled without a verdict in the proof's unbounded `Command::output` capture. Replaced that capture with the existing `homeboy_core::test_support::bounded_output` helper and added phase evidence. This removes a separate unbounded subprocess wait rather than introducing another supervision mechanism.
- Replayed the private-daemon proof against that exact CI-built binary with `HOMEBOY_TEST_SUBPROCESS_BUDGET_SECS=45`: **1 passed, 0 failed**, in **16.45 seconds**. Retained log: `native-proof-276a0898b-bounded-supervisor.log`.
- First cancellation and same-key replay both reported failed/requested/nonterminal with the actual controller owner and recovery command. After the controlled owner exited, the selected run became cancelled; the second lifecycle root stayed queued. The ledger retained `action.failed`, not a false `action.succeeded`, and post-cancellation submission calls remained zero.
- The proof's controller/runner fixtures are private and disposable. No active operator workload was cancelled. A separate published-runtime upgrade correctly refused replacement while unrelated durable work was live; that refusal is not bypassed by this verification.
- AI assistance: OpenAI **gpt-6.1-sol** through **OpenCode** independently reviewed the cancellation boundaries, reproduced the retained operational state, replaced the native proof's unbounded capture, and ran this exact-binary supervisor replay.
