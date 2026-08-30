//! Verdict rules exercised over synthetic candidate/baseline pairs.
//!
//! The classifier is pure, so every rule — including the ones that only appear
//! when a suite is killed mid-run or when a runner writes no sidecar at all — is
//! reachable without executing a single real test. That is deliberate: this
//! module orchestrates test runs, and a design that could only be verified by
//! running them would be untestable on exactly the constrained hosts it exists
//! to serve.

use super::*;
use serde_json::json;

fn counts(total: u64, passed: u64, failed: u64) -> TestCounts {
    TestCounts::new(total, passed, failed, 0)
}

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn evidence(measurement: TestMeasurement) -> BaselineEvidence {
    BaselineEvidence {
        reference: "origin/main".to_string(),
        revision: "1a2b3c4de5f60718293a4b5c6d7e8f9012345678".to_string(),
        recorded_at: "2026-08-06T12:00:00Z".to_string(),
        measurement,
    }
}

fn input(candidate: TestMeasurement, baseline: Option<BaselineEvidence>) -> DifferentialInput {
    DifferentialInput {
        reference: "origin/main".to_string(),
        revision: Some("1a2b3c4de5f60718293a4b5c6d7e8f9012345678".to_string()),
        candidate,
        baseline,
    }
}

fn verdict(candidate: TestMeasurement, baseline: Option<BaselineEvidence>) -> DifferentialVerdict {
    classify(input(candidate, baseline)).verdict
}

// ---------------------------------------------------------------------------
// pass
// ---------------------------------------------------------------------------

/// A green candidate never enters the gate, mirroring the CI script's
/// `if status not in {"fail", "timeout"}: continue`.
#[test]
fn a_green_candidate_passes_without_consulting_the_baseline() {
    let report = classify(input(TestMeasurement::passed(counts(202, 202, 0)), None));

    assert_eq!(report.verdict, DifferentialVerdict::Pass);
    assert!(!report.blocks());
    assert!(report.new_failures.is_empty());
}

/// Strictly fewer failures than a baseline that named none of them is the one
/// count-only improvement the CI script accepts as `pass`.
#[test]
fn fewer_failures_than_an_unnamed_baseline_passes() {
    let candidate = TestMeasurement::failed(counts(200, 198, 2), Vec::new());
    let baseline = evidence(TestMeasurement::failed(counts(200, 195, 5), Vec::new()));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::Pass);
    assert_eq!(report.basis, ComparisonBasis::FailureCounts);
    assert!(!report.blocks());
}

/// Zero findings on a *changed-scope* run is the measurement, not the absence
/// of one, so the zero guard must not fire. This is the entire point of
/// changed-scope gating.
#[test]
fn a_scoped_run_reporting_zero_introduced_failures_passes() {
    let candidate =
        TestMeasurement::failed(counts(0, 0, 0), Vec::new()).with_metric_kind(MetricKind::Scoped);
    let baseline = evidence(TestMeasurement::failed(counts(200, 197, 3), Vec::new()));

    assert_eq!(
        verdict(candidate, Some(baseline)),
        DifferentialVerdict::Pass
    );
}

// ---------------------------------------------------------------------------
// baseline_red
// ---------------------------------------------------------------------------

/// The headline case from #11753: identical counts, identical names, branch
/// clean. This must not render as `pass` — a scope that is red on the base
/// branch stays red forever if no run ever says so.
#[test]
fn identical_named_failures_are_baseline_red_not_pass() {
    let failures = names(&["a::one", "b::two", "c::three"]);
    let candidate = TestMeasurement::failed(counts(205, 202, 3), failures.clone());
    let baseline = evidence(TestMeasurement::failed(counts(200, 197, 3), failures));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::BaselineRed);
    assert_eq!(report.basis, ComparisonBasis::TestNames);
    assert!(!report.blocks());
    assert!(report.new_failures.is_empty());
    assert_eq!(report.inherited_failures.len(), 3);
}

/// `current == base && base > 0` is `baseline_red` even without names. Equal
/// counts with something still red is not an improvement.
#[test]
fn equal_unnamed_failure_counts_are_baseline_red() {
    let candidate = TestMeasurement::failed(counts(205, 202, 3), Vec::new());
    let baseline = evidence(TestMeasurement::failed(counts(200, 197, 3), Vec::new()));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::BaselineRed);
    assert_eq!(report.basis, ComparisonBasis::FailureCounts);
}

/// Names strictly refine counts: fixing one inherited failure while the rest
/// still reproduce is honestly `baseline_red`, not a clean `pass` that hides a
/// still-red scope. It stays non-blocking, so nothing that was mergeable
/// becomes blocked.
#[test]
fn fixing_some_inherited_failures_is_still_baseline_red() {
    let candidate = TestMeasurement::failed(counts(205, 203, 2), names(&["a::one", "b::two"]));
    let baseline = evidence(TestMeasurement::failed(
        counts(200, 197, 3),
        names(&["a::one", "b::two", "c::three"]),
    ));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::BaselineRed);
    assert!(!report.blocks());
    assert_eq!(report.fixed_failures, names(&["c::three"]));
    assert!(report.new_failures.is_empty());
}

/// A baseline that failed before producing counts still licenses `baseline_red`
/// — but only because the candidate *did* produce an observation for the claim
/// to rest on.
#[test]
fn an_unmeasured_baseline_with_a_measured_candidate_is_baseline_red() {
    let candidate = TestMeasurement::failed(counts(205, 202, 3), names(&["a::one"]));
    let baseline = evidence(TestMeasurement::unmeasured(RunOutcome::Failed, 101));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::BaselineRed);
    assert!(!report.blocks());
}

// ---------------------------------------------------------------------------
// fail
// ---------------------------------------------------------------------------

/// Three failed before and three failed after can still be three *different*
/// tests. Every count-only rule calls this `baseline_red`; comparing by name is
/// the only thing that catches it.
#[test]
fn equal_counts_with_a_swapped_failure_set_is_a_regression() {
    let candidate = TestMeasurement::failed(
        counts(205, 202, 3),
        names(&["a::one", "b::two", "z::brand_new"]),
    );
    let baseline = evidence(TestMeasurement::failed(
        counts(200, 197, 3),
        names(&["a::one", "b::two", "c::three"]),
    ));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::Fail);
    assert!(report.blocks());
    assert_eq!(report.new_failures, names(&["z::brand_new"]));
    assert_eq!(report.fixed_failures, names(&["c::three"]));
}

#[test]
fn more_failures_than_an_unnamed_baseline_is_a_regression() {
    let candidate = TestMeasurement::failed(counts(205, 200, 5), Vec::new());
    let baseline = evidence(TestMeasurement::failed(counts(200, 197, 3), Vec::new()));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::Fail);
    assert!(report.blocks());
}

#[test]
fn a_new_failure_against_a_green_baseline_is_a_regression() {
    let candidate = TestMeasurement::failed(counts(205, 204, 1), names(&["z::brand_new"]));
    let baseline = evidence(TestMeasurement::passed(counts(200, 200, 0)));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::Fail);
    assert_eq!(report.new_failures, names(&["z::brand_new"]));
}

// ---------------------------------------------------------------------------
// timeout
// ---------------------------------------------------------------------------

/// A timeout against a healthy baseline keeps blocking. A killed suite usually
/// reports *fewer* failures than the baseline, so a naive `current <= base`
/// would read "the suite never finished" as a clean sweep.
#[test]
fn a_timeout_against_a_healthy_baseline_keeps_blocking() {
    let baseline = evidence(TestMeasurement::passed(counts(200, 200, 0)));

    let report = classify(input(TestMeasurement::timed_out(), Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::Timeout);
    assert!(report.blocks());
}

/// Partial counts from a killed run are not comparable either, even though they
/// look like an improvement.
#[test]
fn a_timeout_with_partial_counts_still_blocks() {
    let candidate = TestMeasurement {
        outcome: RunOutcome::TimedOut,
        exit_code: TIMEOUT_EXIT_CODE,
        invalid_evidence: false,
        ..TestMeasurement::failed(counts(40, 40, 0), Vec::new())
    };
    let baseline = evidence(TestMeasurement::failed(counts(200, 197, 3), Vec::new()));

    assert_eq!(
        verdict(candidate, Some(baseline)),
        DifferentialVerdict::Timeout
    );
}

// ---------------------------------------------------------------------------
// inconclusive
// ---------------------------------------------------------------------------

/// A run that failed while reporting zero failures has an uncounted failure.
/// Compile errors and harness crashes land here.
#[test]
fn a_failure_reporting_zero_failures_is_inconclusive() {
    let candidate = TestMeasurement::failed(counts(0, 0, 0), Vec::new());
    let baseline = evidence(TestMeasurement::failed(counts(200, 197, 3), Vec::new()));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::Inconclusive);
    assert!(!report.blocks());
}

/// A healthy baseline plus an unmeasurable candidate leaves nothing comparable
/// on one side.
#[test]
fn an_unmeasured_candidate_against_a_healthy_baseline_is_inconclusive() {
    let candidate = TestMeasurement::unmeasured(RunOutcome::Failed, 101);
    let baseline = evidence(TestMeasurement::passed(counts(200, 200, 0)));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::Inconclusive);
    assert!(!report.blocks());
}

// ---------------------------------------------------------------------------
// no_measurement
// ---------------------------------------------------------------------------

/// Both sides ran out of clock. Nothing at all is known, and reporting that as
/// `baseline_red` would overstate it — `baseline_red` claims the failure is
/// pre-existing, and that claim needs a candidate-side observation.
#[test]
fn a_double_timeout_is_no_measurement_not_baseline_red() {
    let baseline = evidence(TestMeasurement::timed_out());

    let report = classify(input(TestMeasurement::timed_out(), Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::NoMeasurement);
    assert!(!report.blocks());
}

#[test]
fn two_unmeasured_sides_are_no_measurement() {
    let candidate = TestMeasurement::unmeasured(RunOutcome::Failed, 101);
    let baseline = evidence(TestMeasurement::unmeasured(RunOutcome::Failed, 101));

    assert_eq!(
        verdict(candidate, Some(baseline)),
        DifferentialVerdict::NoMeasurement
    );
}

// ---------------------------------------------------------------------------
// terminal sidecar evidence
// ---------------------------------------------------------------------------

#[test]
fn terminal_sidecar_normalizes_adapter_order_deterministically() {
    let evidence = normalize_terminal_test_evidence(&json!({
        "inventory": [{"id": "suite::b"}, {"id": "suite::a"}],
        "outcomes": [
            {"id": "suite::b", "outcome": "failed"},
            {"id": "suite::a", "outcome": "passed"}
        ]
    }))
    .expect("valid adapter evidence");

    assert_eq!(
        evidence
            .inventory
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["suite::a", "suite::b"]
    );
    assert_eq!(
        evidence
            .outcomes
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["suite::a", "suite::b"]
    );
}

#[test]
fn missing_or_malformed_terminal_sidecars_are_typed_invalid_evidence() {
    for payload in [None, Some(json!({"inventory": []})), Some(json!([]))] {
        let measurement = measurement_from_terminal_test_evidence(payload.as_ref());
        let report = classify(input(
            measurement,
            Some(evidence(TestMeasurement::passed(counts(1, 1, 0)))),
        ));
        assert_eq!(report.verdict, DifferentialVerdict::InvalidEvidence);
        assert!(report.blocks());
    }
}

#[test]
fn duplicate_terminal_identities_are_invalid_evidence() {
    let measurement = measurement_from_terminal_test_evidence(Some(&json!({
        "inventory": [{"id": "same"}, {"id": "same"}],
        "outcomes": [{"id": "same", "outcome": "failed"}]
    })));
    assert!(measurement.invalid_evidence);
}

#[test]
fn candidate_only_terminal_outcomes_are_invalid_evidence() {
    let measurement = measurement_from_terminal_test_evidence(Some(&json!({
        "inventory": [{"id": "known"}],
        "outcomes": [
            {"id": "known", "outcome": "passed"},
            {"id": "candidate-only", "outcome": "failed"}
        ]
    })));
    assert!(measurement.invalid_evidence);
}

#[test]
fn terminal_sidecar_without_a_comparable_inventory_is_invalid_evidence() {
    let measurement = measurement_from_terminal_test_evidence(Some(&json!({
        "inventory": [{"id": "known"}],
        "outcomes": []
    })));
    assert!(measurement.invalid_evidence);
}

// ---------------------------------------------------------------------------
// no_baseline
// ---------------------------------------------------------------------------

/// The honest local degradation. An absent measurement must never render as a
/// pass, so this blocks and says exactly what was missing.
#[test]
fn a_red_candidate_without_a_cached_baseline_is_no_baseline_and_blocks() {
    let candidate = TestMeasurement::failed(counts(205, 202, 3), names(&["a::one"]));

    let report = classify(input(candidate, None));

    assert_eq!(report.verdict, DifferentialVerdict::NoBaseline);
    assert!(report.blocks());
    assert_eq!(report.basis, ComparisonBasis::Unavailable);
}

/// Without a baseline nothing is known about which failures are new, so the
/// candidate's failures must not be reported as new ones.
#[test]
fn no_baseline_does_not_invent_new_failures() {
    let candidate = TestMeasurement::failed(counts(205, 202, 3), names(&["a::one", "b::two"]));

    let report = classify(input(candidate, None));

    assert!(report.new_failures.is_empty());
    assert!(report.inherited_failures.is_empty());
}

// ---------------------------------------------------------------------------
// comparison basis
// ---------------------------------------------------------------------------

/// A partial name list must not drive a set difference: the unnamed failures
/// would look like they disappeared, inventing a fix that did not happen.
#[test]
fn a_partial_name_list_degrades_to_count_comparison() {
    let candidate = TestMeasurement::failed(counts(205, 202, 3), names(&["a::one"]));
    let baseline = evidence(TestMeasurement::failed(
        counts(200, 197, 3),
        names(&["a::one", "b::two", "c::three"]),
    ));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.basis, ComparisonBasis::FailureCounts);
    assert_eq!(report.verdict, DifferentialVerdict::BaselineRed);
    assert!(report.new_failures.is_empty());
}

#[test]
fn failure_names_are_sorted_and_deduplicated() {
    let candidate =
        TestMeasurement::failed(counts(205, 203, 2), names(&["b::two", "a::one", "b::two"]));

    let report = classify(input(candidate, None));

    assert_eq!(report.candidate.failed_tests, names(&["a::one", "b::two"]));
}

// ---------------------------------------------------------------------------
// blocking policy
// ---------------------------------------------------------------------------

/// Non-blocking verdicts are exactly the ones where the candidate is not
/// answerable for the condition. Pinned so a later edit cannot quietly make an
/// absent measurement clear a branch.
#[test]
fn only_regression_timeout_and_missing_baseline_block() {
    for verdict in [
        DifferentialVerdict::Pass,
        DifferentialVerdict::BaselineRed,
        DifferentialVerdict::Inconclusive,
        DifferentialVerdict::NoMeasurement,
    ] {
        assert!(!verdict.blocks(), "{} must not block", verdict.as_str());
    }
    for verdict in [
        DifferentialVerdict::Fail,
        DifferentialVerdict::Timeout,
        DifferentialVerdict::NoBaseline,
        DifferentialVerdict::InvalidEvidence,
    ] {
        assert!(verdict.blocks(), "{} must block", verdict.as_str());
    }
}

/// The wire spellings are the CI gate's, not a second vocabulary.
#[test]
fn verdict_spellings_match_the_ci_gate() {
    assert_eq!(DifferentialVerdict::BaselineRed.as_str(), "baseline_red");
    assert_eq!(DifferentialVerdict::Inconclusive.as_str(), "inconclusive");
    assert_eq!(
        DifferentialVerdict::NoMeasurement.as_str(),
        "no_measurement"
    );
    assert_eq!(DifferentialVerdict::Pass.as_str(), "pass");
    assert_eq!(DifferentialVerdict::Fail.as_str(), "fail");
    assert_eq!(DifferentialVerdict::Timeout.as_str(), "timeout");
    assert_eq!(DifferentialVerdict::NoBaseline.as_str(), "no_baseline");
}

/// The serialized spelling and the rendered spelling must be the same string.
/// Two spellings for one verdict is exactly the "second vocabulary" this module
/// exists to avoid, and it is the kind of drift nothing else would catch.
#[test]
fn serialized_and_rendered_verdict_spellings_agree() {
    for verdict in [
        DifferentialVerdict::Pass,
        DifferentialVerdict::Fail,
        DifferentialVerdict::Timeout,
        DifferentialVerdict::BaselineRed,
        DifferentialVerdict::Inconclusive,
        DifferentialVerdict::NoMeasurement,
        DifferentialVerdict::NoBaseline,
        DifferentialVerdict::InvalidEvidence,
    ] {
        let serialized = serde_json::to_string(&verdict).expect("serialize verdict");
        assert_eq!(serialized, format!("\"{}\"", verdict.as_str()));
    }
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

#[test]
fn the_rendered_block_names_both_sides_and_the_new_failure_count() {
    let failures = names(&["a::one", "b::two", "c::three"]);
    let candidate = TestMeasurement::failed(counts(205, 202, 3), failures.clone());
    let baseline = evidence(TestMeasurement::failed(counts(200, 197, 3), failures));

    let rendered = classify(input(candidate, Some(baseline))).render();

    assert!(
        rendered.contains("candidate: 202 passed, 3 failed"),
        "{rendered}"
    );
    assert!(
        rendered.contains("baseline:  197 passed, 3 failed"),
        "{rendered}"
    );
    assert!(
        rendered.contains("(cached, origin/main @ 1a2b3c4de"),
        "{rendered}"
    );
    assert!(rendered.contains("verdict:   baseline_red"), "{rendered}");
    assert!(rendered.contains("new failures: 0"), "{rendered}");
}

/// A missing baseline must be visible as missing, never blank.
#[test]
fn the_rendered_block_says_when_no_baseline_is_cached() {
    let candidate = TestMeasurement::failed(counts(205, 202, 3), names(&["a::one"]));

    let rendered = classify(input(candidate, None)).render();

    assert!(
        rendered.contains("none cached for origin/main"),
        "{rendered}"
    );
    assert!(rendered.contains("verdict:   no_baseline"), "{rendered}");
}

#[test]
fn the_rendered_block_lists_new_failures() {
    let candidate = TestMeasurement::failed(counts(205, 204, 1), names(&["z::brand_new"]));
    let baseline = evidence(TestMeasurement::passed(counts(200, 200, 0)));

    let rendered = classify(input(candidate, Some(baseline))).render();

    assert!(rendered.contains("new failures: 1"), "{rendered}");
    assert!(rendered.contains("- z::brand_new"), "{rendered}");
}

// ---------------------------------------------------------------------------
// cache
// ---------------------------------------------------------------------------

fn key(revision: &str, scope: &str) -> BaselineCacheKey {
    BaselineCacheKey::new("homeboy", "origin/main", revision, scope)
}

#[test]
fn a_stored_measurement_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let stored = TestMeasurement::failed(counts(200, 197, 3), names(&["b::two", "a::one"]));

    cache
        .store(
            &key("abc123", "-p homeboy-core --lib"),
            &stored,
            "2026-08-06T12:00:00Z",
        )
        .expect("store baseline");
    let loaded = cache
        .load(&key("abc123", "-p homeboy-core --lib"))
        .expect("cached baseline");

    assert_eq!(loaded.revision, "abc123");
    assert_eq!(loaded.reference, "origin/main");
    assert_eq!(
        loaded.measurement.failed_tests,
        names(&["a::one", "b::two"])
    );
}

/// The whole value of the cache: a branch cut from the same base sha reuses the
/// measurement, so the expensive half is paid once per base-branch movement.
#[test]
fn a_second_branch_at_the_same_revision_hits_the_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let measurement = TestMeasurement::failed(counts(200, 197, 3), Vec::new());

    cache
        .store(
            &key("abc123", WHOLE_SUITE_SCOPE),
            &measurement,
            "2026-08-06T12:00:00Z",
        )
        .expect("store baseline");

    assert!(cache.load(&key("abc123", WHOLE_SUITE_SCOPE)).is_some());
}

/// A base branch that moved must be a miss, not a stale answer.
#[test]
fn a_moved_base_revision_is_a_cache_miss() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let measurement = TestMeasurement::failed(counts(200, 197, 3), Vec::new());

    cache
        .store(
            &key("abc123", WHOLE_SUITE_SCOPE),
            &measurement,
            "2026-08-06T12:00:00Z",
        )
        .expect("store baseline");

    assert!(cache.load(&key("def456", WHOLE_SUITE_SCOPE)).is_none());
}

/// A scoped measurement cannot answer for a whole-suite run, or vice versa.
#[test]
fn a_different_scope_is_a_cache_miss() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let measurement = TestMeasurement::failed(counts(200, 197, 3), Vec::new());

    cache
        .store(
            &key("abc123", "-p homeboy-core --lib"),
            &measurement,
            "2026-08-06T12:00:00Z",
        )
        .expect("store baseline");

    assert!(cache.load(&key("abc123", "-p homeboy-cli --lib")).is_none());
    assert!(cache.load(&key("abc123", WHOLE_SUITE_SCOPE)).is_none());
}

/// A record whose contents disagree with the path it was found at has been
/// corrupted or hand-edited. Trusting the path over the contents would turn
/// that into a silent wrong answer.
#[test]
fn a_record_whose_identity_disagrees_with_its_path_is_a_miss() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let requested = key("abc123", WHOLE_SUITE_SCOPE);

    cache
        .store(
            &requested,
            &TestMeasurement::failed(counts(200, 197, 3), Vec::new()),
            "2026-08-06T12:00:00Z",
        )
        .expect("store baseline");

    let path = cache.path_for(&requested);
    let body = std::fs::read_to_string(&path).expect("read record");
    std::fs::write(&path, body.replace("\"abc123\"", "\"tampered\"")).expect("rewrite record");

    assert!(cache.load(&requested).is_none());
}

#[test]
fn an_unknown_schema_is_a_miss() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let requested = key("abc123", WHOLE_SUITE_SCOPE);

    cache
        .store(
            &requested,
            &TestMeasurement::failed(counts(200, 197, 3), Vec::new()),
            "2026-08-06T12:00:00Z",
        )
        .expect("store baseline");

    let path = cache.path_for(&requested);
    let body = std::fs::read_to_string(&path).expect("read record");
    std::fs::write(
        &path,
        body.replace(
            &format!("\"schema\": {BASELINE_CACHE_SCHEMA}"),
            "\"schema\": 999",
        ),
    )
    .expect("rewrite record");

    assert!(cache.load(&requested).is_none());
}

#[test]
fn unreadable_and_absent_records_are_misses_not_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let requested = key("abc123", WHOLE_SUITE_SCOPE);

    assert!(cache.load(&requested).is_none());

    let path = cache.path_for(&requested);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
    std::fs::write(&path, "{ not json").expect("write junk");

    assert!(cache.load(&requested).is_none());
}

/// A cache miss must reach the verdict layer as `no_baseline`, which blocks.
/// A corrupted cache degrades to "prove it yourself", never to a clean verdict.
#[test]
fn a_cache_miss_produces_a_blocking_no_baseline_verdict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());

    let report = classify_against_cache(
        &cache,
        &key("abc123", WHOLE_SUITE_SCOPE),
        TestMeasurement::failed(counts(205, 202, 3), names(&["a::one"])),
    );

    assert_eq!(report.verdict, DifferentialVerdict::NoBaseline);
    assert!(report.blocks());
}

/// The end-to-end shape a command uses: record once at a base revision, then
/// every branch cut from that revision reads the verdict without paying for a
/// second measurement.
#[test]
fn a_recorded_baseline_answers_a_later_branch_from_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let requested = key("abc123", "-p homeboy-core --lib");
    let failures = names(&["a::one", "b::two", "c::three"]);

    cache
        .store(
            &requested,
            &TestMeasurement::failed(counts(200, 197, 3), failures.clone()),
            "2026-08-06T12:00:00Z",
        )
        .expect("store baseline");

    let report = classify_against_cache(
        &cache,
        &requested,
        TestMeasurement::failed(counts(205, 202, 3), failures),
    );

    assert_eq!(report.verdict, DifferentialVerdict::BaselineRed);
    assert!(!report.blocks());
    assert!(report.new_failures.is_empty());
    assert!(report.render().contains("new failures: 0"));
}

/// The base branch moves constantly, so without pruning the cache grows one
/// directory per observed sha forever.
#[test]
fn pruning_keeps_only_the_current_revision() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let measurement = TestMeasurement::failed(counts(200, 197, 3), Vec::new());

    for revision in ["abc123", "def456", "aaa999"] {
        cache
            .store(
                &key(revision, WHOLE_SUITE_SCOPE),
                &measurement,
                "2026-08-06T12:00:00Z",
            )
            .expect("store baseline");
    }

    let removed = cache
        .prune_superseded(&key("aaa999", WHOLE_SUITE_SCOPE))
        .expect("prune");

    assert_eq!(removed, 2);
    assert!(cache.load(&key("aaa999", WHOLE_SUITE_SCOPE)).is_some());
    assert!(cache.load(&key("abc123", WHOLE_SUITE_SCOPE)).is_none());
}

#[test]
fn pruning_an_empty_cache_is_not_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());

    assert_eq!(
        cache
            .prune_superseded(&key("abc123", WHOLE_SUITE_SCOPE))
            .expect("prune"),
        0
    );
}

/// Different scopes must land in different files even after sanitization has
/// collapsed their punctuation to underscores.
#[test]
fn distinct_scopes_get_distinct_file_names() {
    let one = key("abc123", "-p homeboy-core --lib");
    let two = key("abc123", "-p homeboy-cli --lib");

    assert_ne!(one.scope_fingerprint(), two.scope_fingerprint());
    assert_ne!(one.relative_path(), two.relative_path());
}

#[test]
fn scope_keys_drop_empty_arguments_and_default_to_the_whole_suite() {
    let no_args: Vec<String> = Vec::new();
    assert_eq!(scope_key(&no_args), WHOLE_SUITE_SCOPE);
    assert_eq!(
        scope_key(&["  ".to_string(), String::new()]),
        WHOLE_SUITE_SCOPE
    );
    assert_eq!(
        scope_key(&[
            " -p ".to_string(),
            "homeboy-core".to_string(),
            "--lib".to_string()
        ]),
        "-p homeboy-core --lib"
    );
}

/// Cache file names must be usable on any filesystem: the scope is arbitrary
/// runner-argument text.
#[test]
fn cache_paths_are_filesystem_safe() {
    let path = key("abc/../123", "-p a/b --lib=*").relative_path();
    let rendered = path.to_string_lossy();

    assert!(!rendered.contains(".."), "{rendered}");
    assert!(!rendered.contains('*'), "{rendered}");
    assert_eq!(path.components().count(), 3, "{rendered}");
}
