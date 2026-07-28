//! `measurement_ok` — the shared precondition for rendering a green verdict
//! (#10685).
//!
//! # The invariant
//!
//! **Absence of evidence is never evidence of success.** Before any gate reads
//! its verdict, it must be able to point at proof that it measured something.
//! A gate that cannot prove it measured must report `unknown` — never `pass`.
//!
//! # Why this is shared
//!
//! Three places in this repository had independently grown a local version of
//! this rule, and they disagreed with one another about what to do when the
//! instrument came back empty:
//!
//! * `homeboy-extension/src/test/run.rs::test_run_status` had the best of them
//!   — "a zero count or an all-skipped result proves only that the runner
//!   started" — and failed closed to `"failed"`.
//! * `homeboy-code-audit/src/engine.rs` (#10557 / #10574) hard-errors on an
//!   empty corpus, because an audit that scanned zero of 1,817 files had been
//!   reporting `passed: true` in seven seconds for weeks.
//! * `homeboy-extension/src/lint/run/workflow.rs` had none, and still renders
//!   an unconditional `passed` when changed-file scoping resolves to zero
//!   runnable scopes — including when the changed set was *not* empty.
//!
//! This module is the one place those three answers are reconciled. It is
//! deliberately behaviour-free apart from the classification itself: it decides
//! what a measurement *permits*, and leaves the caller to decide what it wants.
//!
//! # What this module does NOT do
//!
//! It classifies the **presence** of a measurement. It says nothing about
//! whether a measurement that exists was *interpreted* correctly. #10657 —
//! where a differential gate rewrote `current == base == 1` to `pass` — is a
//! comparison-semantics defect with fully accurate counts on both sides, and no
//! measurement-presence predicate can catch it. Do not reach for this module
//! expecting it to.

/// Whether the run that produced a measurement got to finish.
///
/// Checked *before* any count-based branch. A killed child usually never writes
/// its results sidecar, so it arrives with absent or partial counts; reading
/// those counts as a total is how a truncated run gets a verdict it did not
/// earn. #10644 established this ordering for the test phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunCompletion {
    /// The instrument ran to completion and had the opportunity to report
    /// everything it saw.
    Complete,
    /// The run was cut short. Whatever it reported is a prefix, not a total.
    Incomplete(IncompleteReason),
}

/// How a run was cut short. Reported verbatim so an operator can tell a budget
/// exhaustion apart from an OOM kill apart from a lost output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncompleteReason {
    /// The run exhausted an execution budget.
    TimedOut,
    /// The process was terminated by a signal.
    Killed,
    /// The run's output was cut off, so counts read from it are a prefix.
    Truncated,
    /// The run did not finish and the cause was not established. Still not a
    /// pass: an unexplained early exit is strictly less reliable than an
    /// explained one.
    Unspecified,
}

impl IncompleteReason {
    /// Operator-facing label.
    pub fn label(self) -> &'static str {
        match self {
            IncompleteReason::TimedOut => "timed out",
            IncompleteReason::Killed => "was killed",
            IncompleteReason::Truncated => "was truncated",
            IncompleteReason::Unspecified => "did not finish",
        }
    }
}

/// What the instrument reported about the amount of work it observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    /// Structured counts survived: `units` units of work were observed.
    ///
    /// A "unit" is whatever the gate counts as evidence it did its job: an
    /// executed test, a fingerprinted source file, a linted file, a validated
    /// release asset. Zero is a legal value and is handled explicitly — it is
    /// not the same as [`Observed::Unreported`].
    Units(u64),
    /// The instrument produced no structured counts at all. There is nothing to
    /// read, which is materially different from reading a zero.
    Unreported,
}

/// The set of candidate units the instrument was pointed at, when that is known
/// from a source *other than the instrument itself*.
///
/// The independence matters. A broken instrument reporting "I saw zero files"
/// is indistinguishable from an honest zero unless something else can say how
/// many files there were. `git diff --name-only` knows the changed set; a
/// directory walk knows the file count; a release plan knows the asset list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Population {
    /// Independently established: `n` candidate units existed.
    Known(u64),
    /// Not independently established. An observed zero then cannot be told
    /// apart from a broken instrument, which is exactly why it is not a pass.
    Unestablished,
}

/// A single gate's claim about what it measured.
///
/// Build with [`Measurement::units`], [`Measurement::unreported`] or
/// [`Measurement::advisory`], refine with [`Measurement::against_population`]
/// and [`Measurement::incomplete`], then call [`Measurement::assess`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measurement {
    observed: Observed,
    population: Population,
    completion: RunCompletion,
    advisory: bool,
}

impl Measurement {
    /// The instrument reported `units` units of observed work.
    pub fn units(units: u64) -> Self {
        Self {
            observed: Observed::Units(units),
            population: Population::Unestablished,
            completion: RunCompletion::Complete,
            advisory: false,
        }
    }

    /// The instrument produced no structured counts at all.
    pub fn unreported() -> Self {
        Self {
            observed: Observed::Unreported,
            population: Population::Unestablished,
            completion: RunCompletion::Complete,
            advisory: false,
        }
    }

    /// An **advisory** measurement: informative, and permitted to move a verdict
    /// in neither direction.
    ///
    /// This exists because the opposite mistake is just as damaging as a false
    /// green. `bounded_directory_size` shells out to `du -sk`; when that failed
    /// the size came back `None`, liveness degraded to `"unknown"`, and prune
    /// became a silent no-op — an advisory instrument silently disabling a real
    /// operation. #10662 set the discipline: a failed advisory measurement
    /// changes the verdict in neither direction. [`MeasurementOutcome::Advisory`]
    /// is inert by construction, so a caller cannot accidentally gate on it.
    pub fn advisory() -> Self {
        Self {
            observed: Observed::Unreported,
            population: Population::Unestablished,
            completion: RunCompletion::Complete,
            advisory: true,
        }
    }

    /// Record an independently-known candidate population.
    ///
    /// This is what upgrades "I observed zero and cannot say why" from
    /// [`MeasurementOutcome::Unmeasured`] to either [`MeasurementOutcome::EmptyPopulation`]
    /// (an honest zero) or [`MeasurementOutcome::Contradicted`] (a provably
    /// broken instrument). Supply it wherever it is cheaply available.
    pub fn against_population(mut self, population: u64) -> Self {
        self.population = Population::Known(population);
        self
    }

    /// Record that the run producing this measurement did not finish.
    pub fn incomplete(mut self, reason: IncompleteReason) -> Self {
        self.completion = RunCompletion::Incomplete(reason);
        self
    }

    /// Classify the measurement. This is the predicate.
    ///
    /// Branch order is load-bearing:
    ///
    /// 1. **Advisory** first, so an advisory instrument that timed out still
    ///    moves nothing.
    /// 2. **Incompleteness** before counts, because a truncated run's counts are
    ///    a prefix and reading them as a total is #10644's defect.
    /// 3. **Unreported** before zero, because "nothing to read" and "read a
    ///    zero" are different states with different remedies.
    /// 4. **Zero against a known population** before zero in general, because
    ///    only the former is provable.
    pub fn assess(&self) -> MeasurementOutcome {
        if self.advisory {
            return MeasurementOutcome::Advisory;
        }
        if let RunCompletion::Incomplete(reason) = self.completion {
            return MeasurementOutcome::Unmeasured(UnmeasuredReason::RunIncomplete(reason));
        }
        let units = match self.observed {
            Observed::Unreported => {
                return MeasurementOutcome::Unmeasured(UnmeasuredReason::NoStructuredCounts);
            }
            Observed::Units(units) => units,
        };
        if units > 0 {
            // Observing more units than the recorded population is not a
            // green-safety problem — the population is a lower bound supplied
            // for the zero case — so it is deliberately not an error here.
            return MeasurementOutcome::Measured { units };
        }
        match self.population {
            Population::Known(0) => MeasurementOutcome::EmptyPopulation,
            Population::Known(population) => MeasurementOutcome::Contradicted { population },
            Population::Unestablished => {
                MeasurementOutcome::Unmeasured(UnmeasuredReason::ZeroUnits)
            }
        }
    }
}

/// Why a measurement cannot support a `pass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmeasuredReason {
    /// The instrument produced no structured counts.
    NoStructuredCounts,
    /// The instrument observed zero units and nothing can say whether zero was
    /// the right answer.
    ZeroUnits,
    /// The run did not finish, so its counts are a prefix.
    RunIncomplete(IncompleteReason),
}

/// The classification of a measurement, and the only thing a gate may consult
/// before rendering `pass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasurementOutcome {
    /// Positive evidence: a run that finished observed at least one unit.
    Measured { units: u64 },
    /// The instrument observed zero and an independent source confirms there
    /// was nothing to observe. An honest zero, and a legitimate `pass`.
    EmptyPopulation,
    /// No usable evidence, and no way to tell an honest zero from a broken
    /// instrument. `unknown` is the honest verdict; `pass` is forbidden.
    Unmeasured(UnmeasuredReason),
    /// The instrument observed zero against a population independently known to
    /// be non-empty. This is not `unknown` — the instrument is provably broken,
    /// and a broken instrument is a hard error.
    Contradicted { population: u64 },
    /// Advisory. Constrains nothing, in either direction.
    Advisory,
}

impl MeasurementOutcome {
    /// **The invariant.** `false` means no verdict-producing path may render
    /// `pass` from this measurement.
    pub fn permits_pass(&self) -> bool {
        match self {
            MeasurementOutcome::Measured { .. }
            | MeasurementOutcome::EmptyPopulation
            | MeasurementOutcome::Advisory => true,
            MeasurementOutcome::Unmeasured(_) | MeasurementOutcome::Contradicted { .. } => false,
        }
    }

    /// A broken instrument. Neither `pass` nor `unknown` is an honest answer, so
    /// callers should surface this as an error rather than a verdict.
    pub fn is_broken_instrument(&self) -> bool {
        matches!(self, MeasurementOutcome::Contradicted { .. })
    }

    /// Constrain an intended verdict to one the measurement can actually
    /// support.
    ///
    /// The downgrade is `Pass -> Unknown`, deliberately **not** `Pass -> Fail`.
    /// Turning every unmeasured gate into a hard failure makes CI permanently
    /// red, and a permanently red gate is ignored — the same end state as the
    /// bug this predicate exists to prevent. `Unknown` is a first-class verdict
    /// here for the same reason #10561 made it one for `EvidenceManifest`: a
    /// gate that could not measure should say so rather than guess in either
    /// direction.
    ///
    /// [`MeasurementOutcome::Contradicted`] is the one case that refuses
    /// outright, because it is the one case where the instrument is *provably*
    /// broken rather than merely silent.
    pub fn constrain(&self, intended: Verdict) -> Result<Verdict, MeasurementRefusal> {
        match self {
            MeasurementOutcome::Contradicted { population } => Err(MeasurementRefusal {
                population: *population,
            }),
            _ if self.permits_pass() => Ok(intended),
            // Unmeasured. A gate may still fail for reasons it *did* establish
            // (a non-zero exit, a parse error); only the green is withheld.
            MeasurementOutcome::Unmeasured(_) => Ok(match intended {
                Verdict::Pass => Verdict::Unknown,
                other => other,
            }),
            // Unreachable: every remaining variant permits a pass.
            _ => Ok(intended),
        }
    }

    /// Operator-facing explanation of why a `pass` was withheld, or `None` when
    /// nothing was withheld.
    ///
    /// Phrased as a statement about the *instrument*, not about the code under
    /// test, so a reader is never misled into debugging their own change.
    pub fn withheld_pass_reason(&self) -> Option<String> {
        match self {
            MeasurementOutcome::Measured { .. }
            | MeasurementOutcome::EmptyPopulation
            | MeasurementOutcome::Advisory => None,
            MeasurementOutcome::Unmeasured(UnmeasuredReason::NoStructuredCounts) => Some(
                "no structured counts were reported, so there is no evidence this gate measured \
                 anything; reporting unknown rather than passed (#10685)"
                    .to_string(),
            ),
            MeasurementOutcome::Unmeasured(UnmeasuredReason::ZeroUnits) => Some(
                "zero units were measured and nothing independently confirms there were none to \
                 measure; reporting unknown rather than passed (#10685)"
                    .to_string(),
            ),
            MeasurementOutcome::Unmeasured(UnmeasuredReason::RunIncomplete(reason)) => {
                Some(format!(
                    "the run {}, so its counts are a prefix rather than a total; reporting unknown \
                     rather than passed (#10685)",
                    reason.label()
                ))
            }
            MeasurementOutcome::Contradicted { population } => Some(format!(
                "measured 0 of {population} candidate unit(s): the instrument is broken, not the \
                 subject. This is a hard error, not an unknown (#10685)"
            )),
        }
    }
}

/// Returned when a measurement is provably broken and no verdict may be
/// rendered from it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasurementRefusal {
    /// The independently-known population the instrument observed zero of.
    pub population: u64,
}

impl std::fmt::Display for MeasurementRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "measured 0 of {} candidate unit(s): a gate that scanned nothing has not passed, it \
             has not run (#10685)",
            self.population
        )
    }
}

/// The three verdicts a gate may render.
///
/// `Unknown` is deliberately distinct from `Fail`: "I could not measure" and "I
/// measured a problem" call for different actions from a reader, and collapsing
/// them destroys the only signal that tells an operator whether to fix the code
/// or fix the instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Unknown,
    Fail,
}

/// Convenience form of the invariant for call sites that only need the boolean.
pub fn measurement_ok(measurement: &Measurement) -> bool {
    measurement.assess().permits_pass()
}

/// A verdict rendered by comparing a candidate measurement against a baseline.
///
/// The rule the issue states is literal and is implemented literally: **a
/// comparison with no structured counts on either side is never `pass`**. Both
/// sides are instruments, and a comparison inherits the weakest of them.
///
/// Advisory sides are ignored, per the advisory contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComparedMeasurement {
    /// The candidate under test.
    pub current: Measurement,
    /// The baseline it is being compared against.
    pub baseline: Measurement,
}

impl ComparedMeasurement {
    pub fn new(current: Measurement, baseline: Measurement) -> Self {
        Self { current, baseline }
    }

    /// The weaker of the two sides' outcomes.
    ///
    /// Severity order, worst first: `Contradicted`, `Unmeasured`,
    /// `EmptyPopulation`/`Measured`, `Advisory`.
    pub fn assess(&self) -> MeasurementOutcome {
        let current = self.current.assess();
        let baseline = self.baseline.assess();
        for outcome in [current, baseline] {
            if outcome.is_broken_instrument() {
                return outcome;
            }
        }
        for outcome in [current, baseline] {
            if matches!(outcome, MeasurementOutcome::Unmeasured(_)) {
                return outcome;
            }
        }
        // Both sides permit a pass. Report the candidate's own outcome, so a
        // caller that wants the unit count gets the candidate's.
        match current {
            MeasurementOutcome::Advisory => baseline,
            other => other,
        }
    }
}

/// Structural guard that a new green-producing gate cannot ship without
/// declaring how it established a measurement. Kept in its own file because it
/// carries the registry of every verdict site in the workspace.
#[cfg(test)]
#[path = "measurement_registry_test.rs"]
mod measurement_registry_test;

#[cfg(test)]
mod tests {
    use super::*;

    // ── The four clauses of the invariant, stated in #10685 ──

    #[test]
    fn zero_units_is_never_a_pass() {
        assert!(!Measurement::units(0).assess().permits_pass());
        assert_eq!(
            Measurement::units(0).assess(),
            MeasurementOutcome::Unmeasured(UnmeasuredReason::ZeroUnits)
        );
    }

    #[test]
    fn a_comparison_with_no_counts_on_either_side_is_never_a_pass() {
        let measured = Measurement::units(7);
        for compared in [
            ComparedMeasurement::new(Measurement::unreported(), measured),
            ComparedMeasurement::new(measured, Measurement::unreported()),
            ComparedMeasurement::new(Measurement::unreported(), Measurement::unreported()),
        ] {
            assert!(
                !compared.assess().permits_pass(),
                "a comparison is only as strong as its weaker side"
            );
        }
        assert!(ComparedMeasurement::new(measured, measured)
            .assess()
            .permits_pass());
    }

    #[test]
    fn zero_units_against_a_non_empty_population_is_a_hard_error() {
        let outcome = Measurement::units(0).against_population(1817).assess();
        assert_eq!(
            outcome,
            MeasurementOutcome::Contradicted { population: 1817 }
        );
        assert!(!outcome.permits_pass());
        assert!(outcome.is_broken_instrument());
        assert!(outcome.constrain(Verdict::Pass).is_err());
        // Even an intended *failure* is refused: nothing this instrument says
        // is trustworthy.
        assert!(outcome.constrain(Verdict::Fail).is_err());
    }

    #[test]
    fn an_incomplete_run_is_never_a_pass_however_healthy_its_partial_counts() {
        for reason in [
            IncompleteReason::TimedOut,
            IncompleteReason::Killed,
            IncompleteReason::Truncated,
            IncompleteReason::Unspecified,
        ] {
            let outcome = Measurement::units(410).incomplete(reason).assess();
            assert_eq!(
                outcome,
                MeasurementOutcome::Unmeasured(UnmeasuredReason::RunIncomplete(reason)),
                "410 passing tests before a kill is a prefix, not a total"
            );
            assert!(!outcome.permits_pass());
        }
    }

    // ── The distinctions the invariant depends on ──

    #[test]
    fn an_independently_confirmed_empty_population_is_an_honest_zero() {
        let outcome = Measurement::units(0).against_population(0).assess();
        assert_eq!(outcome, MeasurementOutcome::EmptyPopulation);
        assert!(
            outcome.permits_pass(),
            "a docs-only checkout with no source files has genuinely nothing to audit"
        );
    }

    #[test]
    fn no_counts_at_all_is_distinct_from_a_counted_zero() {
        assert_eq!(
            Measurement::unreported().assess(),
            MeasurementOutcome::Unmeasured(UnmeasuredReason::NoStructuredCounts)
        );
        assert_eq!(
            Measurement::units(0).assess(),
            MeasurementOutcome::Unmeasured(UnmeasuredReason::ZeroUnits)
        );
        assert_ne!(
            Measurement::unreported().assess().withheld_pass_reason(),
            Measurement::units(0).assess().withheld_pass_reason(),
            "the two states have different remedies and must read differently"
        );
    }

    #[test]
    fn unknown_is_the_downgrade_not_fail() {
        let outcome = Measurement::units(0).assess();
        assert_eq!(outcome.constrain(Verdict::Pass), Ok(Verdict::Unknown));
        // A gate that established a failure by other means keeps it.
        assert_eq!(outcome.constrain(Verdict::Fail), Ok(Verdict::Fail));
        assert_eq!(outcome.constrain(Verdict::Unknown), Ok(Verdict::Unknown));
    }

    #[test]
    fn a_measured_gate_may_render_whatever_it_concluded() {
        let outcome = Measurement::units(1819).assess();
        assert_eq!(outcome, MeasurementOutcome::Measured { units: 1819 });
        for intended in [Verdict::Pass, Verdict::Unknown, Verdict::Fail] {
            assert_eq!(outcome.constrain(intended), Ok(intended));
        }
        assert!(outcome.withheld_pass_reason().is_none());
    }

    // ── Advisory measurements move verdicts in neither direction ──

    #[test]
    fn an_advisory_measurement_constrains_nothing() {
        let outcome = Measurement::advisory().assess();
        assert_eq!(outcome, MeasurementOutcome::Advisory);
        assert!(outcome.permits_pass());
        assert!(outcome.withheld_pass_reason().is_none());
        for intended in [Verdict::Pass, Verdict::Unknown, Verdict::Fail] {
            assert_eq!(
                outcome.constrain(intended),
                Ok(intended),
                "an advisory instrument must not move a verdict in either direction"
            );
        }
    }

    #[test]
    fn an_advisory_measurement_stays_inert_when_it_fails_outright() {
        // The `du -sk` shape: the advisory instrument itself died. It must not
        // degrade the verdict it was decorating.
        let outcome = Measurement::advisory()
            .incomplete(IncompleteReason::Killed)
            .assess();
        assert_eq!(outcome, MeasurementOutcome::Advisory);
        assert_eq!(outcome.constrain(Verdict::Pass), Ok(Verdict::Pass));
    }

    #[test]
    fn an_advisory_side_does_not_weaken_a_comparison() {
        let compared = ComparedMeasurement::new(Measurement::units(3), Measurement::advisory());
        assert!(compared.assess().permits_pass());
        assert_eq!(compared.assess(), MeasurementOutcome::Measured { units: 3 });
    }

    // ── Branch ordering ──

    #[test]
    fn incompleteness_outranks_a_contradicted_population() {
        // A killed run that reports zero against 1817 files is *incomplete*,
        // not a proven-broken instrument: it never got to look.
        let outcome = Measurement::units(0)
            .against_population(1817)
            .incomplete(IncompleteReason::TimedOut)
            .assess();
        assert_eq!(
            outcome,
            MeasurementOutcome::Unmeasured(UnmeasuredReason::RunIncomplete(
                IncompleteReason::TimedOut
            ))
        );
        assert!(!outcome.is_broken_instrument());
    }

    #[test]
    fn a_comparison_reports_the_worst_side_not_the_first() {
        let compared = ComparedMeasurement::new(
            Measurement::unreported(),
            Measurement::units(0).against_population(12),
        );
        assert!(compared.assess().is_broken_instrument());
    }

    // ── Recorded incidents, replayed ──

    #[test]
    fn the_post_merge_audit_gate_incident_cannot_render_green() {
        // #10557: `files_scanned: 0, files_skipped: 1817, findings: [], passed:
        // true` — green in seven seconds on an 1,800-file repository.
        let outcome = Measurement::units(0).against_population(1817).assess();
        assert!(!outcome.permits_pass());
        assert!(
            outcome
                .withheld_pass_reason()
                .expect("a withheld pass must be explained")
                .contains("instrument is broken"),
            "the message must point at the instrument, not at the tree"
        );
    }

    #[test]
    fn the_killed_test_child_incident_cannot_render_green() {
        // #10639/#10644: the child was killed at its 1500s budget before
        // writing its results sidecar, so counts arrived absent.
        let outcome = Measurement::unreported()
            .incomplete(IncompleteReason::TimedOut)
            .assess();
        assert!(!outcome.permits_pass());
        assert_eq!(outcome.constrain(Verdict::Pass), Ok(Verdict::Unknown));
    }

    #[test]
    fn measurement_ok_is_the_boolean_form_of_permits_pass() {
        for measurement in [
            Measurement::units(1),
            Measurement::units(0),
            Measurement::units(0).against_population(0),
            Measurement::units(0).against_population(9),
            Measurement::unreported(),
            Measurement::advisory(),
            Measurement::units(5).incomplete(IncompleteReason::Killed),
        ] {
            assert_eq!(
                measurement_ok(&measurement),
                measurement.assess().permits_pass()
            );
        }
    }
}
