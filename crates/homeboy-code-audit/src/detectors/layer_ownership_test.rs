#[test]
fn test_analyze_layer_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    std::fs::create_dir_all(root.join("inc/Core/Steps")).unwrap();

    std::fs::write(
        root.join("homeboy.json"),
        r#"{
          "audit_rules": {
            "layer_rules": [
              {
                "name": "engine-owns-terminal-status",
                "forbid": {
                  "glob": "inc/Core/Steps/**/*.php",
                  "patterns": ["JobStatus::"]
                },
                "allow": {"glob": "inc/Abilities/Engine/**/*.php"}
              }
            ]
          }
        }"#,
    )
    .unwrap();

    std::fs::write(
        root.join("inc/Core/Steps/agent_ping.php"),
        "<?php\n$status = JobStatus::FAILED;\n",
    )
    .unwrap();

    let findings = super::analyze_layer_ownership(root);
    assert!(findings.iter().any(|f| {
        f.convention == "layer_ownership"
            && f.description.contains("engine-owns-terminal-status")
            && f.description.contains("JobStatus::")
    }));
}
