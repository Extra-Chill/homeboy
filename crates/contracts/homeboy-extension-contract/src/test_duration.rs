//! Pure test *duration* contract types.
//!
//! Duration is a first-class test fact, kept deliberately separate from
//! [`crate::test_result::TestCounts`]. "This test is slow" and "this test
//! failed" are different findings with different owners and different fixes;
//! folding them into one status would make a slow suite indistinguishable from
//! a broken one. Every type here is additive — `TestCounts` is untouched, so
//! previously written baselines and previously published extension payloads
//! keep deserializing unchanged.
//!
//! ## A missing duration is not zero
//!
//! Every measured value is `Option<f64>`. A unit that started but never
//! reported a duration — the normal outcome for the test binary that was
//! executing when a timeout killed the child — carries `seconds: None`. It is
//! excluded from sums, from shares, and from slow evaluation. Coercing an
//! unknown to `0.0` would make the test that consumed the entire budget look
//! like the fastest one in the suite.

use serde::{Deserialize, Serialize};

/// How a duration sample was obtained. Recorded so a reader can tell a
/// directly reported per-test time from an inference.
pub mod duration_source {
    /// A test binary's own `test result: ... finished in Ns` summary.
    pub const BINARY_SUMMARY: &str = "binary-summary";
    /// A per-test time reported by the runner (libtest `--report-time`).
    pub const REPORT_TIME: &str = "report-time";
    /// A per-test time reported by a runner that times individual cases in
    /// its normal output (cargo-nextest does this).
    pub const RUNNER_CASE: &str = "runner-case-timing";
    /// A binary duration attributed to the single test it contains. Exact, not
    /// a guess: with one test in the binary the two are the same measurement.
    pub const SOLE_TEST: &str = "sole-test-attribution";
    /// libtest's built-in `has been running for over N seconds` notice. Names a
    /// slow test without giving its duration, so it yields a lower bound only.
    pub const LONG_RUNNING_NOTICE: &str = "long-running-notice";
}

/// Output markers of the test-runner formats Homeboy can time.
///
/// These literals live in the contract rather than in the parser because they
/// are *data describing a runner's output shape*, not engine behaviour. Keeping
/// them here is what lets the timing engine stay ecosystem-agnostic: it matches
/// declared markers, and a new runner format is a new constant rather than a
/// new branch of hardcoded strings.
pub mod output_marker {
    /// Precedes a test binary's aggregate result line.
    pub const BINARY_SUMMARY: &str = "test result:";
    /// Introduces the elapsed time inside a binary summary line.
    pub const ELAPSED: &str = "finished in ";
    /// Announces the test target about to execute.
    pub const TARGET_START: &str = "Running ";
    /// Announces a documentation-test target.
    pub const DOC_TARGET_START: &str = "Doc-tests ";
    /// Separates a case name from its outcome.
    pub const CASE_OUTCOME: &str = " ... ";
    /// Names a case that has exceeded the runner's own slow threshold. It
    /// carries no duration, only a lower bound.
    pub const LONG_RUNNING: &str = " has been running for over ";
    /// Introduces a whole-suite aggregate with its elapsed time.
    pub const SUITE_SUMMARY: &str = "Summary [";
    /// Introduces a whole-suite elapsed time on its own line.
    pub const SUITE_ELAPSED: &str = "Time: ";
    /// Case outcome keywords that precede a bracketed per-case duration.
    pub const CASE_OUTCOMES: &[&str] = &["PASS", "FAIL", "SLOW", "LEAK", "TIMEOUT"];
}

/// Rule identifiers for duration findings. Distinct from failure findings and
/// from each other: one test hogging the budget and a suite that is uniformly
/// too slow have different fixes.
pub mod slow_rule {
    /// A single test unit consumes an outsized share of the enforced budget.
    pub const SLOW_UNIT: &str = "slow-test-unit";
    /// The suite as a whole is approaching the budget that terminates it.
    pub const SUITE_NEAR_BUDGET: &str = "test-suite-near-budget";
    /// A named test was still running when the run was terminated. The
    /// duration is unknown; only a lower bound is available.
    pub const UNFINISHED_UNIT: &str = "unfinished-test-unit";
}

/// One measured test unit — a test binary, or an individual test case when the
/// runner reports per-test timings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestUnitDuration {
    /// Test binary target path, or test name for a per-test sample.
    pub name: String,
    /// Owning binary when `name` is an individual test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// Wall-clock seconds. `None` means the runner never reported one — the
    /// unit is unmeasured, not instantaneous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    /// Known lower bound in seconds when only a bound is available (a
    /// long-running notice, or a unit killed mid-flight).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_seconds: Option<f64>,
    /// Tests executed by this unit, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<u64>,
    /// Share of the enforced budget, 0.0–1.0. `None` when either the duration
    /// or the budget is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_share: Option<f64>,
    /// Share of the total measured suite time, 0.0–1.0. Context only — the
    /// slow-unit rule keys on `budget_share`, never on this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_share: Option<f64>,
    /// One of [`duration_source`].
    pub source: String,
}

impl TestUnitDuration {
    pub fn new(name: impl Into<String>, source: &str) -> Self {
        Self {
            name: name.into(),
            binary: None,
            seconds: None,
            min_seconds: None,
            tests: None,
            budget_share: None,
            measured_share: None,
            source: source.to_string(),
        }
    }
}

/// A duration finding. Deliberately *not* a [`homeboy_finding::HomeboyFinding`]
/// in the test command's `findings` field: that field feeds test-failure
/// classification, and an advisory measurement must not move that verdict in
/// either direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlowTestFinding {
    /// One of [`slow_rule`].
    pub rule: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_share: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_share: Option<f64>,
    pub message: String,
    /// True when the run that produced this finding did not complete. The
    /// finding is still worth reporting — a killed run is exactly when the
    /// budget hog matters — but it is a partial picture, never a full one.
    #[serde(default)]
    pub incomplete: bool,
}

/// The duration picture for one test phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestDurations {
    /// Wall-clock seconds Homeboy measured for the whole test child, including
    /// compilation and any pre-test phases sharing the same budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_seconds: Option<f64>,
    /// Sum of the per-binary durations that were actually reported. `None`
    /// when nothing was parsed — never `0.0`, which would claim the suite ran
    /// instantly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_seconds: Option<f64>,
    /// The budget this run was armed with, as enforced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_seconds: Option<f64>,
    /// False when the run was terminated or the timing picture is known
    /// partial.
    #[serde(default = "default_complete")]
    pub complete: bool,
    /// Why the picture is incomplete, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_reason: Option<String>,
    /// Per-binary durations, slowest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binaries: Vec<TestUnitDuration>,
    /// Per-test durations where available, slowest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<TestUnitDuration>,
    /// Duration findings. Advisory: they never change the phase verdict.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slow: Vec<SlowTestFinding>,
}

fn default_complete() -> bool {
    true
}

impl Default for TestDurations {
    fn default() -> Self {
        Self {
            phase_seconds: None,
            measured_seconds: None,
            budget_seconds: None,
            complete: true,
            incomplete_reason: None,
            binaries: Vec::new(),
            tests: Vec::new(),
            slow: Vec::new(),
        }
    }
}

impl TestDurations {
    /// True when nothing at all was measured, so the block carries no signal
    /// and should be omitted rather than serialized as a wall of `None`.
    pub fn is_empty(&self) -> bool {
        self.phase_seconds.is_none()
            && self.measured_seconds.is_none()
            && self.binaries.is_empty()
            && self.tests.is_empty()
            && self.slow.is_empty()
    }
}
