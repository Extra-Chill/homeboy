//! Pins nextest shard replay to process-per-test parallelism.
//!
//! `extensions.rust.settings.rust_nextest_shard_threads` is the `--test-threads`
//! value the rust extension passes to `cargo nextest run` during shard replay
//! (`rust/scripts/test-runner.sh`). The extension's own schema states the
//! semantics: "Zero selects nextest's num-cpus parallelism. nextest runs every
//! test in its own process, so process-global state is already isolated and
//! serial replay is only the conservative default."
//!
//! ## Why this needs a test rather than a comment
//!
//! The setting has already regressed once, silently, and the mechanism is worth
//! naming because it will recur.
//!
//! `716370152` ("fix(test): restore parallel hermetic shards") introduced the
//! setting at `0` — that commit exists *only* to turn shard parallelism on, and
//! it updated `docs/internals/test-tiers.md` to say so. `fcc6a5029` ("ci: bound
//! archived test replay") then flipped it to `1`. That commit's subject and its
//! other hunk are about pinning a homeboy-action revision so downloaded Test
//! archives stay out of consumer Git state; the thread change rode along
//! unmentioned. The docs were not updated and continued to document `0`, so for
//! the entire window the documented configuration and the actual configuration
//! disagreed, and the disagreement was invisible.
//!
//! Nothing caught it because the failure mode is not a failure. A serialized
//! suite is *green*. It produces the same results as a parallel one, only
//! slower, so neither CI nor a reviewer reading a diff has a signal to react
//! to. A one-token numeric change inside a config blob is also close to
//! invisible in review when the surrounding commit is about something else.
//! That combination — no runtime signal, low diff salience — is what makes a
//! test the right instrument here rather than a comment or a doc line.
//!
//! ## What is and is not pinned
//!
//! This pins the value to `0` specifically, not merely "not 1". The setting is
//! a thread count, so a well-meaning future edit could set it to `4` or `8` to
//! "bound" resource use, which would re-cap parallelism at an arbitrary number
//! unrelated to the runner's core count. `0` is the only value that delegates
//! to nextest.
//!
//! `rust_cargo_test_threads` is deliberately *not* pinned here. It caps the
//! Cargo fallback runner, which shares one process across threads and therefore
//! genuinely does need serialization while `HomeGuard` still mutates
//! process-global environment (#7505). The two settings look similar and are
//! not: one is a workaround for a real in-process race, the other is a
//! conservative default for a runner that has no such race. Conflating them is
//! how the parallel setting gets "fixed" back to serial.
//!
//! If shard parallelism is ever intentionally retired, delete this test in the
//! same commit that changes the value, so the intent is recorded once rather
//! than argued twice.

use serde_json::Value;

fn rust_settings() -> Value {
    let config: Value =
        serde_json::from_str(include_str!("../homeboy.json")).expect("homeboy.json parses");

    config["extensions"]["rust"]["settings"].clone()
}

#[test]
fn nextest_shard_replay_uses_process_per_test_parallelism() {
    let settings = rust_settings();

    let threads = settings
        .get("rust_nextest_shard_threads")
        .and_then(Value::as_u64)
        .expect("extensions.rust.settings.rust_nextest_shard_threads is present and numeric");

    assert_eq!(
        threads, 0,
        "rust_nextest_shard_threads must stay 0 so nextest shard replay uses its \
         num-cpus process-per-test parallelism. A non-zero value caps replay at a \
         fixed thread count; 1 serializes the suite outright, which is green and \
         therefore silent. See this file's module docs for the regression this \
         guards (716370152 set 0, fcc6a5029 reverted it to 1 unmentioned)."
    );
}

#[test]
fn nextest_is_the_selected_runner() {
    let settings = rust_settings();

    // The shard-threads pin above is meaningless under the Cargo runner, which
    // ignores it. Pinning the runner keeps the two facts from drifting apart:
    // a silent switch back to Cargo would leave a passing parallelism test
    // guarding a setting nothing reads.
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

    // Deliberately the opposite pin from the nextest one, and for the opposite
    // reason. libtest shares a process across its threads, so a test that
    // mutates HOME through HomeGuard can be observed mid-window by a reader on
    // another thread. Until #7505 removes process-global mutation, the Cargo
    // fallback must stay at one thread.
    assert_eq!(
        settings
            .get("rust_cargo_test_threads")
            .and_then(Value::as_u64),
        Some(1),
        "rust_cargo_test_threads must stay 1 while HomeGuard mutates process-global \
         environment (#7505); the Cargo runner shares one process across threads, so \
         env writers can race readers. This is not the same knob as \
         rust_nextest_shard_threads and must not be aligned with it."
    );
}
