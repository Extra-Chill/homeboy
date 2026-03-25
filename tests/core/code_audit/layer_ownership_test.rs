use homeboy::code_audit::audit_path;
use std::path::PathBuf;

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-layer-ownership-{name}-{nanos}"))
}

#[test]
fn test_analyze_layer_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    std::fs::create_dir_all(root.join(".homeboy")).unwrap();
    std::fs::create_dir_all(root.join("inc/Core/Steps")).unwrap();

    std::fs::write(
        root.join(".homeboy/audit-rules.json"),
        r#"{
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
        }"#,
    )
    .unwrap();

    std::fs::write(
        root.join("inc/Core/Steps/agent_ping.php"),
        "<?php\n$status = JobStatus::FAILED;\n",
    )
    .unwrap();

    let result = audit_path(root.to_str().unwrap()).unwrap();
    assert!(result.findings.iter().any(|f| {
        f.convention == "layer_ownership"
            && f.description
                .contains("engine-owns-terminal-status")
            && f.description.contains("JobStatus::")
    }));

    #[test]
    fn test_run_default_path() {

        let result = run();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

}
