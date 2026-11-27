use serde_json::Value;

use crate::{compare_fuzz_hotspot_cohorts, FuzzHotspotCohortItem};

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
fn compares_cohorts_as_measurements_without_gate_semantics() {
    let comparison = compare_fuzz_hotspot_cohorts(
        "baseline",
        "candidate",
        &[item("stable", 10.0, 1), item("resolved", 4.0, 2)],
        &[item("stable", 15.0, 2), item("new", 7.0, 1)],
    );

    assert_eq!(comparison.item_count, 3);
    assert_eq!(comparison.new_items, 1);
    assert_eq!(comparison.resolved_items, 1);
    assert_eq!(comparison.increased_items, 1);
    assert_eq!(comparison.decreased_items, 0);

    let delta = |key: &str| {
        comparison
            .items
            .iter()
            .find(|item| item.key == key)
            .unwrap_or_else(|| panic!("{key} delta"))
    };

    let stable = delta("stable");
    assert_eq!(stable.change_kind, "increased");
    assert_eq!(stable.score_delta, Some(5.0));
    assert_eq!(stable.relative_lift, Some(0.5));
    assert_eq!(stable.normalized_score_delta, Some(5.0 / 15.0));
    assert_eq!(stable.rank_movement, Some(-1));

    // New and resolved items have no baseline to lift from.
    let new_item = delta("new");
    assert_eq!(new_item.change_kind, "new");
    assert_eq!(new_item.relative_lift, None);
    assert_eq!(new_item.normalized_score_delta, Some(1.0));

    let resolved = delta("resolved");
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
                for gate_key in [
                    "status",
                    "classification",
                    "threshold",
                    "pass",
                    "passed",
                    "fail",
                    "failed",
                ] {
                    assert_ne!(key, gate_key);
                }
                assert_no_nested_gate_fields(value);
            }
        }
        Value::Array(items) => items.iter().for_each(assert_no_nested_gate_fields),
        _ => {}
    }
}
