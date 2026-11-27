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
fn a_new_failure_against_a_green_baseline_is_a_regression() {
    let candidate = TestMeasurement::failed(counts(205, 204, 1), names(&["z::brand_new"]));
    let baseline = evidence(TestMeasurement::passed(counts(200, 200, 0)));

    let report = classify(input(candidate, Some(baseline)));

    assert_eq!(report.verdict, DifferentialVerdict::Fail);
    assert_eq!(report.new_failures, names(&["z::brand_new"]));
}

/// Verdict-only rules. Each row is one classification rule; blocking policy
/// per verdict is pinned separately below.
#[test]
fn verdict_rules_over_synthetic_pairs() {
    let killed_with_partial_counts = TestMeasurement {
        outcome: RunOutcome::TimedOut,
        exit_code: TIMEOUT_EXIT_CODE,
        invalid_evidence: false,
        ..TestMeasurement::failed(counts(40, 40, 0), Vec::new())
    };
    let cases: Vec<(
        &str,
        TestMeasurement,
        Option<TestMeasurement>,
        DifferentialVerdict,
    )> = vec![
        // Zero findings on a changed-scope run is the measurement, not the
        // absence of one, so the zero guard must not fire.
        (
            "scoped zero",
            TestMeasurement::failed(counts(0, 0, 0), Vec::new())
                .with_metric_kind(MetricKind::Scoped),
            Some(TestMeasurement::failed(counts(200, 197, 3), Vec::new())),
            DifferentialVerdict::Pass,
        ),
        // `current == base && base > 0` is baseline_red even without names.
        (
            "equal unnamed counts",
            TestMeasurement::failed(counts(205, 202, 3), Vec::new()),
            Some(TestMeasurement::failed(counts(200, 197, 3), Vec::new())),
            DifferentialVerdict::BaselineRed,
        ),
        // An unmeasured baseline licenses baseline_red only because the
        // candidate produced an observation.
        (
            "unmeasured baseline",
            TestMeasurement::failed(counts(205, 202, 3), names(&["a::one"])),
            Some(TestMeasurement::unmeasured(RunOutcome::Failed, 101)),
            DifferentialVerdict::BaselineRed,
        ),
        (
            "more unnamed failures",
            TestMeasurement::failed(counts(205, 200, 5), Vec::new()),
            Some(TestMeasurement::failed(counts(200, 197, 3), Vec::new())),
            DifferentialVerdict::Fail,
        ),
        // A killed suite usually reports fewer failures than the baseline;
        // a naive `current <= base` would read it as a clean sweep.
        (
            "timeout vs healthy",
            TestMeasurement::timed_out(),
            Some(TestMeasurement::passed(counts(200, 200, 0))),
            DifferentialVerdict::Timeout,
        ),
        (
            "timeout with partial counts",
            killed_with_partial_counts,
            Some(TestMeasurement::failed(counts(200, 197, 3), Vec::new())),
            DifferentialVerdict::Timeout,
        ),
        // Failed while reporting zero failures: compile errors, harness crashes.
        (
            "failed with zero failures",
            TestMeasurement::failed(counts(0, 0, 0), Vec::new()),
            Some(TestMeasurement::failed(counts(200, 197, 3), Vec::new())),
            DifferentialVerdict::Inconclusive,
        ),
        (
            "unmeasured candidate vs healthy",
            TestMeasurement::unmeasured(RunOutcome::Failed, 101),
            Some(TestMeasurement::passed(counts(200, 200, 0))),
            DifferentialVerdict::Inconclusive,
        ),
        // Both sides ran out of clock: baseline_red would overstate it.
        (
            "double timeout",
            TestMeasurement::timed_out(),
            Some(TestMeasurement::timed_out()),
            DifferentialVerdict::NoMeasurement,
        ),
        (
            "two unmeasured",
            TestMeasurement::unmeasured(RunOutcome::Failed, 101),
            Some(TestMeasurement::unmeasured(RunOutcome::Failed, 101)),
            DifferentialVerdict::NoMeasurement,
        ),
    ];
    for (label, candidate, baseline, expected) in cases {
        assert_eq!(
            verdict(candidate, baseline.map(evidence)),
            expected,
            "{label}"
        );
    }
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
fn inconsistent_terminal_sidecars_are_invalid_evidence() {
    for payload in [
        // Duplicate identities.
        json!({
            "inventory": [{"id": "same"}, {"id": "same"}],
            "outcomes": [{"id": "same", "outcome": "failed"}]
        }),
        // Outcome for a test absent from the inventory.
        json!({
            "inventory": [{"id": "known"}],
            "outcomes": [
                {"id": "known", "outcome": "passed"},
                {"id": "candidate-only", "outcome": "failed"}
            ]
        }),
        // No comparable outcomes for the inventory.
        json!({
            "inventory": [{"id": "known"}],
            "outcomes": []
        }),
    ] {
        let measurement = measurement_from_terminal_test_evidence(Some(&payload));
        assert!(measurement.invalid_evidence, "{payload}");
    }
}

// ---------------------------------------------------------------------------
// no_baseline
// ---------------------------------------------------------------------------

/// The honest local degradation. An absent measurement must never render as a
/// pass, so this blocks, says exactly what was missing, and does not invent
/// new or inherited failures it cannot know about.
#[test]
fn a_red_candidate_without_a_cached_baseline_is_no_baseline_and_blocks() {
    let candidate = TestMeasurement::failed(counts(205, 202, 3), names(&["a::one", "b::two"]));

    let report = classify(input(candidate, None));

    assert_eq!(report.verdict, DifferentialVerdict::NoBaseline);
    assert!(report.blocks());
    assert_eq!(report.basis, ComparisonBasis::Unavailable);
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

    let candidate = TestMeasurement::failed(counts(205, 204, 1), names(&["z::brand_new"]));
    let baseline = evidence(TestMeasurement::passed(counts(200, 200, 0)));
    let rendered = classify(input(candidate, Some(baseline))).render();
    assert!(rendered.contains("new failures: 1"), "{rendered}");
    assert!(rendered.contains("- z::brand_new"), "{rendered}");
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

/// A branch cut from the same base sha reuses the measurement; a moved base
/// revision or a different scope must be a miss, never a stale answer.
#[test]
fn cache_hits_only_on_matching_revision_and_scope() {
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

    assert!(cache
        .load(&key("abc123", "-p homeboy-core --lib"))
        .is_some());
    assert!(cache
        .load(&key("def456", "-p homeboy-core --lib"))
        .is_none());
    assert!(cache.load(&key("abc123", "-p homeboy-cli --lib")).is_none());
    assert!(cache.load(&key("abc123", WHOLE_SUITE_SCOPE)).is_none());
}

/// Absent, unreadable, tampered (identity disagrees with its path) and
/// unknown-schema records are all misses, never errors or wrong answers.
#[test]
fn absent_or_corrupted_records_are_misses_not_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BaselineCache::at(dir.path());
    let requested = key("abc123", WHOLE_SUITE_SCOPE);
    assert!(cache.load(&requested).is_none());

    let schema = format!("\"schema\": {BASELINE_CACHE_SCHEMA}");
    let corruptions: [&dyn Fn(String) -> String; 3] = [
        &|_| "{ not json".to_string(),
        &|body| body.replace("\"abc123\"", "\"tampered\""),
        &|body| body.replace(&schema, "\"schema\": 999"),
    ];
    for corrupt in corruptions {
        cache
            .store(
                &requested,
                &TestMeasurement::failed(counts(200, 197, 3), Vec::new()),
                "2026-08-06T12:00:00Z",
            )
            .expect("store baseline");
        assert!(cache.load(&requested).is_some());

        let path = cache.path_for(&requested);
        let body = std::fs::read_to_string(&path).expect("read record");
        std::fs::write(&path, corrupt(body)).expect("rewrite record");

        assert!(cache.load(&requested).is_none());
    }
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
    assert_eq!(
        cache
            .prune_superseded(&key("abc123", WHOLE_SUITE_SCOPE))
            .expect("pruning an empty cache is not an error"),
        0
    );

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
