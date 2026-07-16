use super::super::*;
use super::*;

#[test]
fn compare_triage_observations_reports_new_resolved_and_changed_items() {
    let previous = vec![
        stored_triage_item(1, "Old issue", None),
        stored_triage_item(2, "Resolved issue", None),
        stored_triage_item(3, "Changed PR", Some("review_required")),
    ];
    let current = vec![
        new_triage_item("current-run", 1, "Old issue", None),
        new_triage_item("current-run", 3, "Changed PR", Some("checks_failed")),
        new_triage_item("current-run", 4, "New issue", None),
    ];

    let comparison = compare_triage_observations("previous-run", &previous, &current);

    assert_eq!(comparison.previous_run_id, "previous-run");
    assert_eq!(comparison.previous_item_count, 3);
    assert_eq!(comparison.new_items.len(), 1);
    assert_eq!(comparison.new_items[0].number, 4);
    assert_eq!(comparison.resolved_items.len(), 1);
    assert_eq!(comparison.resolved_items[0].number, 2);
    assert_eq!(comparison.changed_items.len(), 1);
    assert_eq!(comparison.changed_items[0].item.number, 3);
    assert_eq!(
        comparison.changed_items[0].changed_fields,
        vec!["next_action"]
    );
}

#[test]
fn triage_observation_metadata_distinguishes_personal_and_firehose_runs() {
    let personal = TriageOptions {
        mine: true,
        ..TriageOptions::default()
    };
    let firehose = TriageOptions {
        mine: false,
        ..TriageOptions::default()
    };

    assert_ne!(
        triage_observation_metadata(&TriageTarget::Workspace, &personal),
        triage_observation_metadata(&TriageTarget::Workspace, &firehose)
    );
}

#[test]
fn compare_triage_observations_ignores_unknown_merge_state_flaps() {
    let mut previous = stored_triage_item(1, "Flappy PR", Some("checks_failed"));
    previous.signals.merge_state = Some("UNKNOWN".to_string());
    let mut current = new_triage_item("current-run", 1, "Flappy PR", Some("checks_failed"));
    current.signals.merge_state = Some("DIRTY".to_string());

    let comparison = compare_triage_observations("previous-run", &[previous], &[current]);

    assert!(comparison.changed_items.is_empty());
}
