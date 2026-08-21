//! Pins nextest shard replay to serial execution.
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
//! ## What has to be true before this goes back to `0`
//!
//! Not "the flakes stopped". The stores have to stop being machine-global:
//! controller admission and the rig registry need the same explicit-root
//! treatment #7505 is applying everywhere else, so that a test's writes land in
//! its own `HermeticTestContext` rather than in a directory every concurrent
//! test can see.
//!
//! Until then a parallel value is not faster, it is red. Serial is slower and
//! correct, and slower-and-correct is the side to err on for a gate whose
//! entire job is to be believed.
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
//! If shard parallelism is intentionally restored, delete or invert this test in
//! the same commit that changes the value, so the intent is recorded once rather
//! than argued a fourth time.

use serde_json::Value;

fn rust_settings() -> Value {
    let config: Value =
        serde_json::from_str(include_str!("../homeboy.json")).expect("homeboy.json parses");

    config["extensions"]["rust"]["settings"].clone()
}

#[test]
fn nextest_shard_replay_stays_serial_while_stores_are_machine_global() {
    let settings = rust_settings();

    let threads = settings
        .get("rust_nextest_shard_threads")
        .and_then(Value::as_u64)
        .expect("extensions.rust.settings.rust_nextest_shard_threads is present and numeric");

    assert_eq!(
        threads, 1,
        "rust_nextest_shard_threads must stay 1 while the controller-runtime \
         admission store and the rig registry live in machine-global directories. \
         nextest gives every test its own process, which isolates env but not the \
         filesystem those stores sit on, so concurrent tests observe each other's \
         leases and registry writes. See this file's module docs for the observed \
         cross-test contamination (run 32294476539). Restore 0 only after those \
         stores take explicit roots (#7505)."
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
