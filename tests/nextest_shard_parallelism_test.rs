//! Pins nextest shard replay to parallel execution.
//!
//! `extensions.rust.settings.rust_nextest_shard_threads` is the `--test-threads`
//! value the rust extension passes to `cargo nextest run` during shard replay
//! (`rust/scripts/test-runner.sh`).
//!
//! ## Why serial, when nextest already isolates processes
//!
//! The rust extension's schema says of the `0` value: "Zero selects nextest's
//! num-cpus parallelism. nextest runs every test in its own process, so
//! process-global state is already isolated and serial replay is only the
//! conservative default."
//!
//! That statement is true and it is not sufficient, which is the whole reason
//! this file says `1` today and said `0` before.
//!
//! Process-per-test isolates process-global *memory and environment* — the
//! `HOME`/`XDG_*` mutation that `HomeGuard` performs, which is the race #7505
//! exists to remove. It does nothing whatsoever for machine-global *filesystem*
//! state. The controller-runtime admission store and the rig registry are
//! directories on disk beneath the real `$HOME`. Two test processes running at
//! the same time share them completely, and a fresh process is not a fresh
//! disk.
//!
//! This is not a theoretical distinction. With shard replay parallel, the run
//! at `32294476539` failed `status_preserves_existing_terminal_runtime_evidence`
//! on an `assert_eq!(before, after)` whose two sides differed in exactly one
//! field:
//!
//! ```text
//! before  metadata.controller_admission.owner = { pid: 9913, advisory_lock: true,
//!                                                 request_id: "run_store-contract", .. }
//! after   metadata.controller_admission.owner = Null
//! ```
//!
//! `run_store-contract` is the run id of a *different test in this same file*.
//! One test observed another test's admission lease through the shared on-disk
//! controller-runtime store, mid-flight. Process isolation was total and
//! irrelevant. Ten `workspace::tests::prune::*` tests fail the same way through
//! the shared rig registry, where a peer's `create` leaves the registry in a
//! state the assertions were not written for.
//!
//! ## That condition was met, and the value is back at `0`
//!
//! The stores stopped being machine-global. The rig registry reached zero
//! ambient production resolutions across #13011/#13019/#13030/#13032, and
//! controller admission reached zero in #13035, where the ambient entry points
//! were additionally gated `#[cfg(test)]` so production cannot reach them even
//! by accident.
//!
//! Measured on an 8-core Linux host, `cargo nextest`, repeated runs:
//!
//! | scope | serial | parallel |
//! |---|---|---|
//! | `workspace::tests::prune::*` (the 10 named above) | 26.5s, 30/30 | 5.6s, 30/30 |
//! | `status_and_recovery` (holds both tests named above) | 20.3s, 53/53 | 4.7s, 53/53 |
//! | `homeboy-lab-runner --lib` (1881 tests) | 410.3s | 98.5s |
//! | `homeboy-agents --lib` (1946 tests) | — | 164.0s |
//!
//! For both crates the parallel-only failure count is zero: every remaining
//! failure reproduces serially in isolation and is environmental to that host.
//! `homeboy-lab-runner` is a 4.2x improvement.
//!
//! ### Two things had to be fixed first, and neither was a store
//!
//! Restoring parallelism surfaced two defects that serialization had been
//! hiding, which is the argument for not serializing:
//!
//! - `linux_scope_pids` counted *every* unreadable `/proc/<pid>/environ` as
//!   incomplete discovery, including `ENOENT` — a process that exited between
//!   `read_dir` and the read. That is the state cleanup is trying to reach, not
//!   a gap in it. Any concurrent load made managed-service cleanup report
//!   `incomplete` spuriously, in production as much as in tests. Now only
//!   `EPERM`/`EACCES` counts, because only that is a real blind spot.
//! - Two scheduler tests asserted concurrency and non-starvation with total
//!   wall-clock bounds, which restate a structural property as a claim about
//!   host load. Both now assert the property directly: overlapping SIGTERM
//!   handler windows, and the task-start events that were already there.
//!
//! ### Cautions for whoever measures this next
//!
//! Both of these produced false results here before being caught:
//!
//! - A host under disk pressure can have cleanup delete
//!   `/var/tmp/.homeboy-test-tmp` mid-run. That presents as ~1000 failures
//!   reading `No such file or directory`, not as a race. Check free space
//!   before believing a bad run.
//! - `/dev/null` on that host had been replaced by a regular file containing 99
//!   bytes of git error text, which makes `git` fail with `bad config line 1 in
//!   file /dev/null` in whichever tests happen to run while it is broken. That
//!   looks exactly like an intermittent race and is not one.
//!
//! ## Why this is pinned rather than commented
//!
//! The value has now moved three times, and only one of those moves announced
//! itself:
//!
//! - `716370152` ("fix(test): restore parallel hermetic shards") introduced it
//!   at `0` and updated `docs/internals/test-tiers.md` to match.
//! - `fcc6a5029` ("ci: bound archived test replay") flipped it to `1`. That
//!   commit's subject and its other hunk are about pinning a homeboy-action
//!   revision; the thread change rode along unmentioned and the docs were not
//!   updated.
//! - `522c7cda2` (#12626) read that silence as an accident and flipped it back
//!   to `0`, adding this file to hold the line. The reasoning was that nextest's
//!   process isolation made serialization unnecessary — correct about processes,
//!   wrong about the filesystem, and the tests above are the bill for it.
//!
//! The failure mode in both directions is quiet. A serialized suite is green and
//! merely slow, so nothing signals. A parallel suite is red intermittently and
//! in tests that look unrelated to the change under review, so the signal points
//! somewhere else. A one-token numeric edit inside a config blob is close to
//! invisible in review either way. That is what makes a test the right
//! instrument here.
//!
//! Shard parallelism has now been intentionally restored, and this test was
//! inverted in the same commit that changed the value rather than deleted — so a
//! silent re-serialization fails here too. The failure mode this file exists to
//! prevent is a one-token numeric edit that nobody argues about.

use serde_json::Value;

fn rust_settings() -> Value {
    let config: Value =
        serde_json::from_str(include_str!("../homeboy.json")).expect("homeboy.json parses");

    config["extensions"]["rust"]["settings"].clone()
}

#[test]
fn nextest_shard_replay_stays_parallel_now_that_the_stores_take_roots() {
    let settings = rust_settings();

    let threads = settings
        .get("rust_nextest_shard_threads")
        .and_then(Value::as_u64)
        .expect("extensions.rust.settings.rust_nextest_shard_threads is present and numeric");

    assert_eq!(
        threads, 0,
        "rust_nextest_shard_threads must stay 0. It was 1 for a real reason and that \
         reason is gone: the controller-runtime admission store and the rig registry \
         now take explicit roots for every production caller, and both failure \
         families this file documented pass in parallel. Serializing again costs \
         roughly 4x wall clock and buys nothing measured. If a shared-directory race \
         returns, fix the store rather than re-serializing the suite, or invert this \
         test in the same commit and say which store — the one thing this value has \
         never survived is being changed quietly. See this file's module docs and #7505."
    );
}

#[test]
fn nextest_is_the_selected_runner() {
    let settings = rust_settings();

    // The shard-threads pin above is meaningless under the Cargo runner, which
    // ignores it. Pinning the runner keeps the two facts from drifting apart:
    // a silent switch back to Cargo would leave a passing test guarding a
    // setting nothing reads.
    assert_eq!(
        settings.get("rust_test_runner").and_then(Value::as_str),
        Some("nextest"),
        "extensions.rust.settings.rust_test_runner must stay \"nextest\"; \
         rust_nextest_shard_threads is only consumed on the nextest path"
    );
}

#[test]
fn cargo_fallback_stays_serialized_while_home_guard_mutates_process_env() {
    let settings = rust_settings();

    // Same value as the nextest pin now, but for a different reason, and the
    // two must not be collapsed into one rule. libtest shares a process across
    // its threads, so a test mutating HOME through HomeGuard can be observed
    // mid-window by a reader on another thread. That race is in-process and
    // survives even if every store on disk becomes explicitly rooted.
    assert_eq!(
        settings
            .get("rust_cargo_test_threads")
            .and_then(Value::as_u64),
        Some(1),
        "rust_cargo_test_threads must stay 1 while HomeGuard mutates process-global \
         environment (#7505); the Cargo runner shares one process across threads, so \
         env writers can race readers. This is a different race from the one \
         rust_nextest_shard_threads guards and clears on a different condition."
    );
}
