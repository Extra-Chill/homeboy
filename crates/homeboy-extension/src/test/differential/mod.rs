//! Local differential test verdict — "is this red because of my change, or was
//! it already red on the base branch?"
//!
//! # The question this answers
//!
//! `main` carries known-red tests and merges dozens of pull requests a day, so
//! a fresh failure on a branch is as likely to be *inherited* as *authored*.
//! Answering that locally used to cost a second checkout of the base branch and
//! a second test build — several minutes and several gigabytes of `target/` —
//! and most of the time the answer was "your branch is fine". That is pure
//! overhead paid over and over. See Extra-Chill/homeboy#11753.
//!
//! # Why the cache is the feature
//!
//! Comparing against the base branch is only worth doing if the expensive half
//! is paid **once per base-branch movement**, not once per branch. So the
//! baseline measurement is stored keyed by the base revision's sha (see
//! [`cache`]): every branch cut from the same `main` sha reuses one
//! measurement, and a `main` that moved is a cache miss rather than a stale
//! answer.
//!
//! # Why this vocabulary and not a new one
//!
//! `homeboy-action`'s `scripts/core/apply-differential-gate.py` already defines
//! `baseline_red`, `inconclusive`, and `no_measurement`, and it already worked
//! out the non-obvious cases. This module mirrors those meanings exactly rather
//! than inventing a second, subtly different set:
//!
//! * `current == base && base > 0` is **`baseline_red`, not `pass`**. Equal
//!   counts with something still red is not an improvement, and reporting
//!   `pass` renders a green result with no annotation anywhere — so a test that
//!   is red on the base branch stays red forever because no run ever says so.
//! * A **timeout against a healthy baseline keeps blocking**. Counts from a
//!   suite that was killed mid-run are not comparable to counts from one that
//!   finished; a killed run usually reports *fewer* failures (often zero,
//!   because the sidecar was never written), so a naive `current <= base` reads
//!   "the suite never finished" as a clean sweep.
//! * A command that **failed while reporting zero failures** is
//!   `inconclusive`, not `pass`: its own failure went uncounted. Changed-scope
//!   (`Scoped`) metrics are exempt, because there zero *is* the measurement.
//! * When **neither side produced counts**, the verdict is `no_measurement`.
//!   `baseline_red` asserts something specific — that this failure is
//!   pre-existing — and that claim needs an observation on the candidate side
//!   to rest on. A double timeout knows nothing about either side, and dressing
//!   that up as a diagnosis is how absence of evidence becomes evidence of
//!   absence.
//!
//! # What this adds beyond the CI gate
//!
//! * [`DifferentialVerdict::NoBaseline`] — the local-only honest degradation.
//!   The CI gate always has a baseline job; locally there may simply be nothing
//!   cached and no budget to build one. That must never render as a clean
//!   verdict, so it is its own state and it blocks.
//! * **Comparison by test name, not only by count.** Three failures before and
//!   three after can still be three *different* tests, which every count-only
//!   rule reads as `baseline_red`. When both sides name their failures, the
//!   verdict is driven by the set difference and count parity cannot launder a
//!   swapped failure set. Name knowledge strictly refines the count rule and
//!   never turns a non-blocking verdict into a blocking one.

pub mod cache;
mod render;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use homeboy_extension_contract::test_result::TestCounts;
use homeboy_extension_contract::test_results::TestCommandOutput;

pub use cache::{
    default_root as default_cache_root, scope_key, BaselineCache, BaselineCacheKey,
    CachedBaselineRecord, BASELINE_CACHE_SCHEMA, BASELINE_CACHE_STORE, WHOLE_SUITE_SCOPE,
};
pub use render::render_report;

/// How a side's failure count was derived.
///
/// Mirrors `SCOPED`/`TOTAL` in `apply-differential-gate.py`. The distinction is
/// load-bearing for the zero guard in [`classify`]: zero means opposite things
/// depending on provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    /// A deliberate measurement of what *this change* introduced. Zero is the
    /// success case: the repository may be red, but the candidate added nothing
    /// to it.
    Scoped,
    /// A raw count of everything the run found. Zero here, on a run that
    /// nonetheless failed, is not a measurement of success — it means the
    /// failure went uncounted.
    Total,
}

/// The terminal state of one side's test invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Passed,
    Failed,
    /// The run exhausted its execution budget and was killed. Neither a test
    /// failure nor a broken harness: the suite is *incomplete*.
    TimedOut,
}

/// One side of a differential comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestMeasurement {
    pub outcome: RunOutcome,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<TestCounts>,
    /// Fully qualified names of the failing tests, when the runner named them.
    /// Sorted and deduplicated by [`TestMeasurement::normalized`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_tests: Vec<String>,
    pub metric_kind: MetricKind,
    /// Whether this run produced parseable counts or a failure list at all.
    ///
    /// Distinct from "reported zero failures": a run that died before writing
    /// its results sidecar is unstructured, and its absent counts must not be
    /// read as a clean sweep.
    pub structured_output: bool,
}

impl TestMeasurement {
    /// A green run.
    pub fn passed(counts: TestCounts) -> Self {
        Self {
            outcome: RunOutcome::Passed,
            exit_code: 0,
            counts: Some(counts),
            failed_tests: Vec::new(),
            metric_kind: MetricKind::Total,
            structured_output: true,
        }
    }

    /// A red run that reported counts, and optionally named its failures.
    pub fn failed(counts: TestCounts, failed_tests: Vec<String>) -> Self {
        Self {
            outcome: RunOutcome::Failed,
            exit_code: 1,
            counts: Some(counts),
            failed_tests,
            metric_kind: MetricKind::Total,
            structured_output: true,
        }
    }

    /// A run that produced no comparable measurement at all — killed, crashed,
    /// or failed before writing a results sidecar.
    pub fn unmeasured(outcome: RunOutcome, exit_code: i32) -> Self {
        Self {
            outcome,
            exit_code,
            counts: None,
            failed_tests: Vec::new(),
            metric_kind: MetricKind::Total,
            structured_output: false,
        }
    }

    /// A run killed by its execution budget.
    pub fn timed_out() -> Self {
        Self::unmeasured(RunOutcome::TimedOut, TIMEOUT_EXIT_CODE)
    }

    pub fn with_metric_kind(mut self, kind: MetricKind) -> Self {
        self.metric_kind = kind;
        self
    }

    pub fn with_exit_code(mut self, exit_code: i32) -> Self {
        self.exit_code = exit_code;
        self
    }

    /// Sort and deduplicate the failure names so set arithmetic and cache
    /// records are order-independent.
    pub fn normalized(mut self) -> Self {
        self.failed_tests.sort();
        self.failed_tests.dedup();
        self
    }

    /// The comparable failure count, or `None` when nothing comparable exists.
    ///
    /// Mirrors `test_count()` in `apply-differential-gate.py`: prefer reported
    /// counts, fall back to the length of the failure list, and report absence
    /// rather than zero when neither is present.
    pub fn failure_count(&self) -> Option<u64> {
        if let Some(counts) = &self.counts {
            return Some(counts.failed);
        }
        if self.structured_output {
            return Some(self.failed_tests.len() as u64);
        }
        None
    }

    /// Whether the named failures fully account for the reported failure count.
    ///
    /// A partial name list degrades to count comparison rather than producing a
    /// set difference over an incomplete set, which would invent regressions.
    pub fn names_account_for_failures(&self) -> bool {
        self.structured_output
            && self
                .failure_count()
                .is_some_and(|count| count == self.failed_tests.len() as u64)
    }

    fn is_green(&self) -> bool {
        self.outcome == RunOutcome::Passed
    }
}

/// A cached measurement of the base branch, with the identity it was taken at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineEvidence {
    /// The ref the caller asked about, e.g. `origin/main`.
    pub reference: String,
    /// The revision `reference` resolved to when the measurement was recorded.
    pub revision: String,
    /// RFC 3339 timestamp of when this measurement was recorded.
    pub recorded_at: String,
    pub measurement: TestMeasurement,
}

impl BaselineEvidence {
    pub fn normalized(mut self) -> Self {
        self.measurement = self.measurement.normalized();
        self
    }

    /// Sliced by character, not byte, so a non-hex identifier cannot panic on a
    /// UTF-8 boundary.
    fn short_revision(&self) -> String {
        self.revision.chars().take(9).collect()
    }
}

/// Everything [`classify`] needs. The reference is carried separately from the
/// baseline so a *missing* baseline can still say what it was missing.
#[derive(Debug, Clone, PartialEq)]
pub struct DifferentialInput {
    /// The base ref the caller asked to compare against.
    pub reference: String,
    /// The revision `reference` currently resolves to, when git could tell us.
    pub revision: Option<String>,
    pub candidate: TestMeasurement,
    pub baseline: Option<BaselineEvidence>,
}

/// The classification vocabulary, mirroring `apply-differential-gate.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DifferentialVerdict {
    /// Nothing to attribute: the candidate is green, or it strictly improved on
    /// a baseline that named no failures the candidate still carries.
    Pass,
    /// The candidate introduced failures the baseline does not have.
    Fail,
    /// The candidate did not finish while the baseline is healthy. Its counts
    /// are not comparable, so this keeps blocking.
    Timeout,
    /// The failures reproduce unchanged on the base branch. Non-blocking:
    /// nothing regressed — but nothing improved either, and saying so is the
    /// whole point.
    BaselineRed,
    /// Counts were unavailable on one side, or the candidate failed while
    /// reporting zero failures. Warns; never accepted as an improvement.
    Inconclusive,
    /// Neither side produced comparable counts. Nothing at all is known — this
    /// is deliberately *not* `baseline_red`, which would overstate it.
    NoMeasurement,
    /// Local-only: no cached baseline exists for this revision and scope, and
    /// none was built. An absent measurement must never render as a pass.
    NoBaseline,
}

impl DifferentialVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Timeout => "timeout",
            Self::BaselineRed => "baseline_red",
            Self::Inconclusive => "inconclusive",
            Self::NoMeasurement => "no_measurement",
            Self::NoBaseline => "no_baseline",
        }
    }

    /// Whether this verdict should stop the caller.
    ///
    /// `baseline_red`, `inconclusive`, and `no_measurement` are non-blocking
    /// for the same reason they are in CI: a candidate is not answerable for a
    /// condition it did not cause. `no_baseline` blocks because it is reached
    /// only with a red candidate and no evidence of inheritance whatsoever —
    /// clearing it would be laundering absence into approval.
    pub fn blocks(self) -> bool {
        matches!(self, Self::Fail | Self::Timeout | Self::NoBaseline)
    }
}

/// Which evidence the verdict actually rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonBasis {
    /// Both sides named every failure they counted; the verdict is a set
    /// difference.
    TestNames,
    /// At least one side did not name its failures; the verdict is a count
    /// comparison, exactly as in CI.
    FailureCounts,
    /// No comparison was possible.
    Unavailable,
}

/// The full differential result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DifferentialReport {
    pub verdict: DifferentialVerdict,
    pub basis: ComparisonBasis,
    pub reference: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_revision: Option<String>,
    pub candidate: TestMeasurement,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselineEvidence>,
    /// Failures present on the candidate and absent from the baseline.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub new_failures: Vec<String>,
    /// Failures present on the baseline and absent from the candidate.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fixed_failures: Vec<String>,
    /// Failures present on both sides.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inherited_failures: Vec<String>,
    pub explanation: String,
}

impl DifferentialReport {
    pub fn blocks(&self) -> bool {
        self.verdict.blocks()
    }

    /// Render the operator-facing verdict block.
    pub fn render(&self) -> String {
        render_report(self)
    }
}

/// Exit status assigned to a child terminated for exhausting its execution
/// budget. Duplicated from `crate::test::report` deliberately: this module must
/// be usable without pulling in the report envelope.
pub const TIMEOUT_EXIT_CODE: i32 = 124;

/// Classify a candidate run against a cached baseline.
///
/// Pure: no filesystem, no git, no clock. Every branch is reachable from
/// synthetic inputs, which is what makes the rules testable without executing a
/// single real test.
pub fn classify(input: DifferentialInput) -> DifferentialReport {
    let DifferentialInput {
        reference,
        revision,
        candidate,
        baseline,
    } = input;

    let candidate = candidate.normalized();
    let baseline = baseline.map(BaselineEvidence::normalized);

    let (new_failures, fixed_failures, inherited_failures) = match baseline.as_ref() {
        // Without a baseline, nothing is known about which failures are new.
        // Reporting the candidate's failures as "new" would assert exactly the
        // thing that could not be measured.
        None => (Vec::new(), Vec::new(), Vec::new()),
        Some(evidence) => diff_names(&candidate, &evidence.measurement),
    };

    let basis = comparison_basis(&candidate, baseline.as_ref());
    let (verdict, explanation) = decide(
        &candidate,
        baseline.as_ref(),
        basis,
        &new_failures,
        &reference,
    );

    DifferentialReport {
        verdict,
        basis,
        reference,
        requested_revision: revision,
        candidate,
        baseline,
        new_failures,
        fixed_failures,
        inherited_failures,
        explanation,
    }
}

/// Resolve the base ref to the revision its cached measurement is keyed by.
///
/// `None` when git cannot resolve it — an unfetched `origin/main`, a detached
/// checkout, or no repository at all. The caller must treat that as a cache
/// miss rather than guessing a revision, because a measurement filed under the
/// wrong sha is worse than no measurement.
pub fn resolve_baseline_revision(repository_root: &Path, reference: &str) -> Option<String> {
    homeboy_core::git::rev_parse(repository_root, reference)
}

/// Look up the cached baseline for `key` and classify `candidate` against it.
///
/// The single call a command needs. A cache miss becomes
/// [`DifferentialVerdict::NoBaseline`] rather than an error, so an unavailable
/// measurement can never be mistaken for a clean one.
pub fn classify_against_cache(
    cache: &BaselineCache,
    key: &BaselineCacheKey,
    candidate: TestMeasurement,
) -> DifferentialReport {
    let baseline = cache.load(key);
    classify(DifferentialInput {
        reference: key.reference.clone(),
        revision: Some(key.revision.clone()),
        candidate,
        baseline,
    })
}

fn comparison_basis(
    candidate: &TestMeasurement,
    baseline: Option<&BaselineEvidence>,
) -> ComparisonBasis {
    let Some(evidence) = baseline else {
        return ComparisonBasis::Unavailable;
    };
    if candidate.names_account_for_failures() && evidence.measurement.names_account_for_failures() {
        ComparisonBasis::TestNames
    } else if candidate.failure_count().is_some() && evidence.measurement.failure_count().is_some()
    {
        ComparisonBasis::FailureCounts
    } else {
        ComparisonBasis::Unavailable
    }
}

/// Set arithmetic over failure names, but only when both sides fully named
/// their failures. A partial list on either side yields empty sets so the
/// count rule stays in charge.
fn diff_names(
    candidate: &TestMeasurement,
    baseline: &TestMeasurement,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    if !(candidate.names_account_for_failures() && baseline.names_account_for_failures()) {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let candidate_set: BTreeSet<String> = candidate.failed_tests.iter().cloned().collect();
    let baseline_set: BTreeSet<String> = baseline.failed_tests.iter().cloned().collect();

    (
        candidate_set.difference(&baseline_set).cloned().collect(),
        baseline_set.difference(&candidate_set).cloned().collect(),
        candidate_set.intersection(&baseline_set).cloned().collect(),
    )
}

fn decide(
    candidate: &TestMeasurement,
    baseline: Option<&BaselineEvidence>,
    basis: ComparisonBasis,
    new_failures: &[String],
    reference: &str,
) -> (DifferentialVerdict, String) {
    // Mirrors `if status not in {"fail", "timeout"}: continue` — a green
    // candidate never enters the gate.
    if candidate.is_green() {
        return (
            DifferentialVerdict::Pass,
            "the candidate suite is green, so there is nothing to attribute".to_string(),
        );
    }

    let timed_out = candidate.outcome == RunOutcome::TimedOut;

    let Some(evidence) = baseline else {
        return (
            DifferentialVerdict::NoBaseline,
            format!(
                "no cached baseline measurement for {reference} at this scope, so nothing was \
                 compared; this is not evidence that the failure is pre-existing"
            ),
        );
    };

    let current = candidate.failure_count();
    let base = evidence.measurement.failure_count();
    let base_failed = !evidence.measurement.is_green() || evidence.measurement.exit_code != 0;
    let base_structured = evidence.measurement.structured_output;

    if base_failed && (base.is_none() || !base_structured) {
        // `baseline_red` asserts this failure is pre-existing, and that claim
        // needs an observation on the candidate side to rest on. A double
        // timeout has none.
        if timed_out || current.is_none() {
            return (
                DifferentialVerdict::NoMeasurement,
                format!(
                    "neither the candidate nor the baseline produced comparable counts (baseline \
                     at {} exited {}); nothing is known about this run",
                    evidence.short_revision(),
                    evidence.measurement.exit_code
                ),
            );
        }
        return (
            DifferentialVerdict::BaselineRed,
            format!(
                "the baseline at {} exited {} before comparable counts were available",
                evidence.short_revision(),
                evidence.measurement.exit_code
            ),
        );
    }

    // Past this point the baseline is healthy, so an incomplete candidate run
    // is the candidate's problem and must keep blocking. This guard precedes
    // every count-based branch: a killed suite typically reports *fewer*
    // failures than the baseline, so `current <= base` would read as an
    // improvement and silently pass.
    if timed_out {
        return (
            DifferentialVerdict::Timeout,
            "the candidate run did not finish, so its counts are not comparable to the baseline; \
             raise the execution budget or reduce suite duration rather than reading this as a \
             test failure"
                .to_string(),
        );
    }

    let (Some(current), Some(base)) = (current, base) else {
        return (
            DifferentialVerdict::Inconclusive,
            format!(
                "counts were unavailable on one side (candidate={}, baseline={})",
                describe_count(current),
                describe_count(base)
            ),
        );
    };

    match basis {
        ComparisonBasis::TestNames => {
            if !new_failures.is_empty() {
                return (
                    DifferentialVerdict::Fail,
                    format!(
                        "{} failure(s) are present here and absent on {reference}: {}",
                        new_failures.len(),
                        new_failures.join(", ")
                    ),
                );
            }
            if current > 0 {
                return (
                    DifferentialVerdict::BaselineRed,
                    format!(
                        "the {current} failure(s) reproduce unchanged on {reference}; nothing \
                         regressed, but this scope is red on the base branch"
                    ),
                );
            }
        }
        _ => {
            if current > base {
                return (
                    DifferentialVerdict::Fail,
                    format!(
                        "failures increased against {reference} (current={current} base={base})"
                    ),
                );
            }
            // Equal counts with something still red is not an improvement, and
            // `pass` is an actively false statement about it.
            if current == base && base > 0 {
                return (
                    DifferentialVerdict::BaselineRed,
                    format!(
                        "{current} failure(s) reproduce unchanged on {reference} (current={current} \
                         base={base}); nothing regressed, but nothing improved either"
                    ),
                );
            }
        }
    }

    // Zero failures on a run that nonetheless failed is not a clean result; it
    // is an uncounted failure. `Scoped` is exempt because there zero *is* the
    // measurement.
    if current == 0 && candidate.metric_kind != MetricKind::Scoped {
        return (
            DifferentialVerdict::Inconclusive,
            format!(
                "the candidate failed but reported 0 failures (baseline={base}), so its own \
                 failure is uncounted; an incomplete, killed, or non-reporting run is never \
                 accepted as an improvement"
            ),
        );
    }

    (
        DifferentialVerdict::Pass,
        format!(
            "the candidate carries fewer failures than {reference} (current={current} base={base})"
        ),
    )
}

fn describe_count(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_string(), |value| value.to_string())
}

/// Project a completed test command envelope into a comparable measurement.
///
/// This is the seam the CLI calls: everything above it is pure, and everything
/// below it is the existing test-run plumbing.
pub fn measurement_from_test_output(output: &TestCommandOutput) -> TestMeasurement {
    let outcome = if output.exit_code == 0 {
        RunOutcome::Passed
    } else if output.exit_code == TIMEOUT_EXIT_CODE {
        RunOutcome::TimedOut
    } else {
        RunOutcome::Failed
    };

    let failed_tests = output
        .summary
        .as_ref()
        .map(|summary| {
            summary
                .failures
                .iter()
                .map(|failure| failure.test_name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // A changed-scope run measures what the change introduced, so its zero is a
    // measurement rather than the absence of one.
    let metric_kind = match output.test_scope.as_ref() {
        Some(scope) if scope.changed_since.is_some() => MetricKind::Scoped,
        _ => MetricKind::Total,
    };

    TestMeasurement {
        outcome,
        exit_code: output.exit_code,
        counts: output.test_counts.clone(),
        failed_tests,
        metric_kind,
        structured_output: output.test_counts.is_some() || output.summary.is_some(),
    }
    .normalized()
}
