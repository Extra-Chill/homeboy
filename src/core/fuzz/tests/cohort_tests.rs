use serde_json::Value;

use crate::core::fuzz::{compare_fuzz_hotspot_cohorts, FuzzHotspotCohortItem};

fn item(key: &str, score: f64, rank: usize) -> FuzzHotspotCohortItem {
    FuzzHotspotCohortItem {
        key: key.to_string(),
        label: None,
        score,
        occurrences: 1,
        run_count: 1,
        rank,
    }
}

#[test]
fn compares_cohorts_without_threshold_or_gate_semantics() {
    let comparison = compare_fuzz_hotspot_cohorts(
        "baseline",
        "candidate",
        &[item("stable", 10.0, 1), item("resolved", 5.0, 2)],
        &[item("stable", 15.0, 2), item("new", 3.0, 1)],
    );

    assert_eq!(comparison.item_count, 3);
    assert_eq!(comparison.new_items, 1);
    assert_eq!(comparison.resolved_items, 1);
    assert_eq!(comparison.increased_items, 1);
    assert_eq!(comparison.decreased_items, 0);

    let stable = comparison
        .items
        .iter()
        .find(|item| item.key == "stable")
        .expect("stable delta");
    assert_eq!(stable.change_kind, "increased");
    assert_eq!(stable.score_delta, Some(5.0));
    assert_eq!(stable.relative_lift, Some(0.5));
    assert_eq!(stable.normalized_score_delta, Some(5.0 / 15.0));
    assert_eq!(stable.rank_movement, Some(-1));

    let serialized = serde_json::to_value(&comparison).expect("serialize comparison");
    assert!(serialized.get("status").is_none());
    assert!(serialized.get("threshold").is_none());
    assert!(serialized.get("passed").is_none());
    assert!(serialized.get("failed").is_none());
}

#[test]
fn reports_new_and_resolved_effects_without_inventing_relative_lift() {
    let comparison = compare_fuzz_hotspot_cohorts(
        "baseline",
        "candidate",
        &[item("resolved", 4.0, 1)],
        &[item("new", 7.0, 1)],
    );

    let new_item = comparison
        .items
        .iter()
        .find(|item| item.key == "new")
        .expect("new delta");
    assert_eq!(new_item.change_kind, "new");
    assert_eq!(new_item.relative_lift, None);
    assert_eq!(new_item.normalized_score_delta, Some(1.0));

    let resolved = comparison
        .items
        .iter()
        .find(|item| item.key == "resolved")
        .expect("resolved delta");
    assert_eq!(resolved.change_kind, "resolved");
    assert_eq!(resolved.relative_lift, None);
    assert_eq!(resolved.normalized_score_delta, Some(-1.0));

    let serialized = serde_json::to_value(&comparison).expect("serialize comparison");
    assert_no_nested_gate_fields(&serialized);
}

fn assert_no_nested_gate_fields(value: &Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                assert_ne!(key, "status");
                assert_ne!(key, "classification");
                assert_ne!(key, "threshold");
                assert_ne!(key, "pass");
                assert_ne!(key, "fail");
                assert_no_nested_gate_fields(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_no_nested_gate_fields(item);
            }
        }
        _ => {}
    }
}
