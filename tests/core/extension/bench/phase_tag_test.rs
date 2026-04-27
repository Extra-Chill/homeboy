//! Phase-tag round-trip + render-grouping smokes for the bench
//! measurement-phase metadata that landed alongside `BenchMetricPhase`
//! / `BenchMetricPolicy.phase` / `BenchComparisonDiff.phase_groups`.
//!
//! The contract under test:
//!
//! 1. **Round-trip:** every `BenchMetricPhase` variant serializes to its
//!    lowercase string form and deserializes back identically.
//! 2. **Back-compat parse:** policy JSON without a `phase` key parses
//!    successfully with `phase: None`.
//! 3. **Back-compat serialize:** `phase: None` is omitted entirely from
//!    the serialized JSON envelope so old consumers see no field
//!    introduction at all.
//! 4. **Render grouping:** when at least one policy declares a phase,
//!    `BenchComparisonDiff::build` populates `phase_groups` with cold
//!    metrics first, then warm, then amortized, then untagged.
//! 5. **No-phase invariant:** when no policy declares a phase,
//!    `phase_groups` is `None` and the JSON envelope is byte-identical
//!    to pre-phase output.
//!
//! Phase is **metadata only** — it never participates in regression
//! math. The render-grouping contract drives table layout for
//! phase-aware report consumers; the `by_scenario` map stays
//! alphabetical for stability across runs.

use std::collections::{BTreeMap, BTreeSet};

use crate::extension::bench::parsing::{
    parse_bench_results_str, BenchMetricDirection, BenchMetricPhase, BenchMetricPolicy,
    BenchMetrics, BenchResults, BenchScenario,
};
use crate::extension::bench::report::{BenchComparisonDiff, BenchPhaseGroups};

fn policy(direction: BenchMetricDirection, phase: Option<BenchMetricPhase>) -> BenchMetricPolicy {
    BenchMetricPolicy {
        direction,
        regression_threshold_percent: None,
        regression_threshold_absolute: None,
        variance_aware: false,
        min_iterations_for_variance: None,
        regression_test: None,
        phase,
    }
}

fn scenario(id: &str, metrics: &[(&str, f64)]) -> BenchScenario {
    let mut values = BTreeMap::new();
    for (k, v) in metrics {
        values.insert((*k).to_string(), *v);
    }
    BenchScenario {
        id: id.to_string(),
        file: None,
        source: None,
        default_iterations: None,
        tags: Vec::new(),
        iterations: 10,
        metrics: BenchMetrics {
            values,
            distributions: BTreeMap::new(),
        },
        memory: None,
    }
}

fn results_with(
    scenarios: Vec<BenchScenario>,
    policies: BTreeMap<String, BenchMetricPolicy>,
) -> BenchResults {
    BenchResults {
        component_id: "demo".to_string(),
        iterations: 10,
        scenarios,
        metric_policies: policies,
    }
}

// 1a. Cold round-trips through serde with the lowercase wire form.
#[test]
fn phase_cold_serializes_lowercase() {
    let p = policy(
        BenchMetricDirection::LowerIsBetter,
        Some(BenchMetricPhase::Cold),
    );
    let raw = serde_json::to_string(&p).unwrap();
    assert!(
        raw.contains("\"phase\":\"cold\""),
        "expected lowercase 'cold', got: {}",
        raw
    );

    let back: BenchMetricPolicy = serde_json::from_str(&raw).unwrap();
    assert_eq!(back.phase, Some(BenchMetricPhase::Cold));
}

// 1b. Warm round-trips.
#[test]
fn phase_warm_serializes_lowercase() {
    let p = policy(
        BenchMetricDirection::LowerIsBetter,
        Some(BenchMetricPhase::Warm),
    );
    let raw = serde_json::to_string(&p).unwrap();
    assert!(
        raw.contains("\"phase\":\"warm\""),
        "expected lowercase 'warm', got: {}",
        raw
    );

    let back: BenchMetricPolicy = serde_json::from_str(&raw).unwrap();
    assert_eq!(back.phase, Some(BenchMetricPhase::Warm));
}

// 1c. Amortized round-trips.
#[test]
fn phase_amortized_serializes_lowercase() {
    let p = policy(
        BenchMetricDirection::LowerIsBetter,
        Some(BenchMetricPhase::Amortized),
    );
    let raw = serde_json::to_string(&p).unwrap();
    assert!(
        raw.contains("\"phase\":\"amortized\""),
        "expected lowercase 'amortized', got: {}",
        raw
    );

    let back: BenchMetricPolicy = serde_json::from_str(&raw).unwrap();
    assert_eq!(back.phase, Some(BenchMetricPhase::Amortized));
}

// 2. Pre-existing JSON without a `phase` key parses with phase=None.
#[test]
fn policy_without_phase_field_parses_as_none() {
    let raw = r#"{
        "component_id": "demo",
        "iterations": 1,
        "metric_policies": {
            "p95_ms": {
                "direction": "lower_is_better",
                "regression_threshold_percent": 5.0
            }
        },
        "scenarios": [
            {
                "id": "s",
                "iterations": 1,
                "metrics": { "p95_ms": 100.0 }
            }
        ]
    }"#;
    let parsed = parse_bench_results_str(raw).unwrap();
    let pol = parsed.metric_policies.get("p95_ms").unwrap();
    assert_eq!(
        pol.phase, None,
        "missing phase field should deserialize as None"
    );
    assert_eq!(pol.direction, BenchMetricDirection::LowerIsBetter);
}

#[test]
fn scenario_source_round_trips_for_rig_workload_origin() {
    let raw = r#"{
        "component_id": "demo",
        "iterations": 1,
        "scenarios": [
            {
                "id": "cold-boot",
                "file": "/private/benches/cold-boot.php",
                "source": "rig",
                "iterations": 1,
                "metrics": { "p95_ms": 100.0 }
            }
        ]
    }"#;

    let parsed = parse_bench_results_str(raw).unwrap();
    let scenario = parsed.scenarios.first().expect("scenario");
    assert_eq!(scenario.source.as_deref(), Some("rig"));

    let serialized = serde_json::to_string(&parsed).unwrap();
    assert!(
        serialized.contains("\"source\":\"rig\""),
        "scenario source should serialize for report consumers: {}",
        serialized
    );
}

// 3. None phase is omitted entirely from the wire form (back-compat
// for consumers that didn't expect a `phase` key).
#[test]
fn policy_serializes_without_phase_field_when_none() {
    let p = policy(BenchMetricDirection::LowerIsBetter, None);
    let raw = serde_json::to_string(&p).unwrap();
    assert!(
        !raw.contains("phase"),
        "phase: None must not appear in serialized output, got: {}",
        raw
    );
}

// 4a. phase_groups is populated when any policy declares a phase, with
// canonical ordering: cold → warm → amortized → untagged.
#[test]
fn phase_groups_orders_cold_before_warm_before_amortized_before_untagged() {
    let mut policies = BTreeMap::new();
    policies.insert(
        "boot_ms".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Cold),
        ),
    );
    policies.insert(
        "p95_ms".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Warm),
        ),
    );
    policies.insert(
        "first_paint_ms".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Amortized),
        ),
    );
    // `error_rate` intentionally has no policy → falls into untagged.

    let ref_r = results_with(
        vec![scenario(
            "page_load",
            &[
                ("boot_ms", 3500.0),
                ("p95_ms", 12.0),
                ("first_paint_ms", 600.0),
                ("error_rate", 0.0),
            ],
        )],
        policies,
    );
    // Comparison rig with the same shape so all metrics appear in the
    // diff (otherwise unmatched metrics drop out per build()'s contract).
    let other = results_with(
        vec![scenario(
            "page_load",
            &[
                ("boot_ms", 3000.0),
                ("p95_ms", 13.0),
                ("first_paint_ms", 580.0),
                ("error_rate", 0.0),
            ],
        )],
        BTreeMap::new(),
    );

    let diff = BenchComparisonDiff::build(("trunk", &ref_r), &[("combined-fixes", &other)]);
    let groups = diff
        .phase_groups
        .as_ref()
        .expect("phase_groups must be Some when any policy declares a phase");

    assert_eq!(groups.cold, vec!["boot_ms".to_string()]);
    assert_eq!(groups.warm, vec!["p95_ms".to_string()]);
    assert_eq!(groups.amortized, vec!["first_paint_ms".to_string()]);
    assert_eq!(groups.untagged, vec!["error_rate".to_string()]);
}

// 4b. Within a phase, metric names are alphabetical for stable render.
#[test]
fn phase_groups_sorts_within_phase_alphabetically() {
    let mut policies = BTreeMap::new();
    // Two cold metrics, declared in non-alphabetical order to prove the
    // bucket sorts independently.
    policies.insert(
        "load_deps_ms".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Cold),
        ),
    );
    policies.insert(
        "boot_ms".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Cold),
        ),
    );

    let ref_r = results_with(
        vec![scenario(
            "init",
            &[("boot_ms", 100.0), ("load_deps_ms", 200.0)],
        )],
        policies,
    );
    let other = results_with(
        vec![scenario(
            "init",
            &[("boot_ms", 110.0), ("load_deps_ms", 190.0)],
        )],
        BTreeMap::new(),
    );

    let diff = BenchComparisonDiff::build(("a", &ref_r), &[("b", &other)]);
    let groups = diff.phase_groups.unwrap();
    assert_eq!(
        groups.cold,
        vec!["boot_ms".to_string(), "load_deps_ms".to_string()]
    );
}

// 5a. No-phase invariant: phase_groups is None when no policy declares
// a phase. This is the "byte-identical to today" guarantee for any
// existing extension that doesn't opt into phase tagging.
#[test]
fn phase_groups_is_none_when_no_policy_declares_a_phase() {
    let mut policies = BTreeMap::new();
    policies.insert(
        "p95_ms".to_string(),
        policy(BenchMetricDirection::LowerIsBetter, None),
    );

    let ref_r = results_with(vec![scenario("scenario", &[("p95_ms", 100.0)])], policies);
    let other = results_with(
        vec![scenario("scenario", &[("p95_ms", 110.0)])],
        BTreeMap::new(),
    );

    let diff = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other)]);
    assert!(
        diff.phase_groups.is_none(),
        "phase_groups must be None when no policy declares a phase"
    );
}

// 5b. No-phase invariant: with no policies at all (the legacy p95-only
// path), phase_groups is also None.
#[test]
fn phase_groups_is_none_when_metric_policies_is_empty() {
    let ref_r = results_with(vec![scenario("s", &[("p95_ms", 100.0)])], BTreeMap::new());
    let other = results_with(vec![scenario("s", &[("p95_ms", 105.0)])], BTreeMap::new());

    let diff = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other)]);
    assert!(diff.phase_groups.is_none());
}

// 5c. JSON envelope back-compat: when phase_groups is None it must be
// completely absent from the serialized form (not `"phase_groups":
// null`). Any consumer that asserted on the exact JSON shape pre-phase
// must continue to pass.
#[test]
fn diff_serializes_without_phase_groups_field_when_phaseless() {
    let ref_r = results_with(vec![scenario("s", &[("p95_ms", 100.0)])], BTreeMap::new());
    let other = results_with(vec![scenario("s", &[("p95_ms", 105.0)])], BTreeMap::new());

    let diff = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other)]);
    let raw = serde_json::to_string(&diff).unwrap();
    assert!(
        !raw.contains("phase_groups"),
        "phase_groups must not appear in JSON when None, got: {}",
        raw
    );
}

// 5d. JSON envelope: when phase_groups is Some, empty buckets are
// omitted (e.g. only cold metrics → warm/amortized/untagged absent
// from JSON).
#[test]
fn phase_groups_omits_empty_buckets_in_json() {
    let mut policies = BTreeMap::new();
    policies.insert(
        "boot_ms".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Cold),
        ),
    );

    let ref_r = results_with(vec![scenario("init", &[("boot_ms", 3500.0)])], policies);
    let other = results_with(
        vec![scenario("init", &[("boot_ms", 3000.0)])],
        BTreeMap::new(),
    );

    let diff = BenchComparisonDiff::build(("a", &ref_r), &[("b", &other)]);
    let raw = serde_json::to_string(&diff).unwrap();
    assert!(raw.contains("\"cold\":[\"boot_ms\"]"), "got: {}", raw);
    assert!(
        !raw.contains("\"warm\""),
        "empty warm bucket must be omitted, got: {}",
        raw
    );
    assert!(
        !raw.contains("\"amortized\""),
        "empty amortized bucket must be omitted, got: {}",
        raw
    );
    assert!(
        !raw.contains("\"untagged\""),
        "empty untagged bucket must be omitted, got: {}",
        raw
    );
}

// 6. Unit test for the BenchPhaseGroups builder: feeding it a
// policies-with-phase table plus a metric-name set produces the
// expected bucketing without going through BenchComparisonDiff.
#[test]
fn bench_phase_groups_from_policies_buckets_correctly() {
    let mut policies = BTreeMap::new();
    policies.insert(
        "boot_ms".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Cold),
        ),
    );
    policies.insert(
        "p95_ms".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Warm),
        ),
    );

    let mut names = BTreeSet::new();
    names.insert("boot_ms".to_string());
    names.insert("p95_ms".to_string());
    names.insert("error_rate".to_string()); // no policy → untagged

    let groups = BenchPhaseGroups::from_policies(&policies, &names);
    assert_eq!(groups.cold, vec!["boot_ms".to_string()]);
    assert_eq!(groups.warm, vec!["p95_ms".to_string()]);
    assert!(groups.amortized.is_empty());
    assert_eq!(groups.untagged, vec!["error_rate".to_string()]);
    assert!(
        !groups.is_phaseless(),
        "must report phaseful when any bucket is non-empty"
    );
}

// 7. by_scenario stays alphabetical even when phase_groups is
// populated. The render-order contract lives in phase_groups; the data
// table stays stable.
#[test]
fn by_scenario_inner_map_stays_alphabetical_when_phase_tagged() {
    let mut policies = BTreeMap::new();
    policies.insert(
        "boot_ms".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Cold),
        ),
    );
    policies.insert(
        "z_warm_metric".to_string(),
        policy(
            BenchMetricDirection::LowerIsBetter,
            Some(BenchMetricPhase::Warm),
        ),
    );

    let ref_r = results_with(
        vec![scenario(
            "init",
            &[("boot_ms", 100.0), ("z_warm_metric", 5.0)],
        )],
        policies,
    );
    let other = results_with(
        vec![scenario(
            "init",
            &[("boot_ms", 110.0), ("z_warm_metric", 6.0)],
        )],
        BTreeMap::new(),
    );

    let diff = BenchComparisonDiff::build(("a", &ref_r), &[("b", &other)]);
    let metric_keys: Vec<String> = diff
        .by_scenario
        .get("init")
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        metric_keys,
        vec!["boot_ms".to_string(), "z_warm_metric".to_string()],
        "by_scenario inner map must stay alphabetical regardless of phase tagging"
    );

    // And phase_groups still encodes the cold-before-warm render order
    // for any consumer that wants it.
    let groups = diff.phase_groups.unwrap();
    assert_eq!(groups.cold, vec!["boot_ms".to_string()]);
    assert_eq!(groups.warm, vec!["z_warm_metric".to_string()]);
}
