use super::AgentTaskGateToolchainRequirement;

#[test]
fn resolve_only_round_trips_and_defaults_false() {
    let typed = AgentTaskGateToolchainRequirement {
        command: "composer".into(),
        probe_arguments: vec!["composer test".into()],
        resolve_only: true,
    };
    let value = serde_json::to_value(&typed).unwrap();
    assert_eq!(value["resolve_only"], true);
    assert_eq!(
        serde_json::from_value::<AgentTaskGateToolchainRequirement>(value).unwrap(),
        typed
    );
    let old: AgentTaskGateToolchainRequirement =
        serde_json::from_value(serde_json::json!({"command": "php", "probe_arguments": ["-v"]}))
            .unwrap();
    assert!(!old.resolve_only);
    assert!(serde_json::to_value(&old)
        .unwrap()
        .get("resolve_only")
        .is_none());
}
