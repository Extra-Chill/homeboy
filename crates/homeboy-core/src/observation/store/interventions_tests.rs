use super::*;

#[test]
fn interventions_are_durable_bounded_and_targeted() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("observations.sqlite");
    let store = ObservationStore::open_initialized_at(&path).unwrap();
    for (target, command) in [
        ("run-1", "agent-task cancel"),
        ("workflow:wf-1", "workflow cancel"),
        ("runner:lab", "runner restart"),
    ] {
        record_intervention(&store, target, command, Some("maintenance")).unwrap();
    }
    // Model progress-window pruning: durable audit payloads must remain readable.
    store
        .connection
        .execute(
            "UPDATE control_plane_event_appends SET event_json = NULL",
            [],
        )
        .unwrap();
    drop(store);
    let store = ObservationStore::open_readonly_at(&path).unwrap();
    let events = store.interventions(None, 100).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["target"], "runner:lab");
    assert_eq!(events[0]["reason"], "maintenance");
    assert_eq!(store.interventions(None, 1).unwrap().len(), 1);
    assert!(store
        .interventions(Some("9999-01-01T00:00:00Z"), 100)
        .unwrap()
        .is_empty());
}
