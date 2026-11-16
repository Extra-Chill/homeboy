use std::process::Command;

/// Exercise real terminal invocation recording and the public friction report.
#[test]
fn successful_materialization_calls_are_not_failure_linked_reruns() {
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str], caller: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_homeboy"));
        command
            .args(args)
            .env("HOME", home.path())
            .env("HOMEBOY_DATA_DIR", home.path().join("data"))
            .env_remove("HOMEBOY_CALLER_SESSION")
            .env_remove("HOMEBOY_CALLER_CONTEXT")
            .env_remove("PI_SESSION_ID")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID");
        if let Some(caller) = caller {
            command.env("HOMEBOY_CALLER_SESSION", caller);
        }
        command.output().unwrap()
    };
    assert!(run(&["runs", "list"], Some("setup")).status.success());
    let input = home.path().join("plan.json");
    let response = home.path().join("response.json");
    std::fs::write(
        &input,
        serde_json::to_vec(&serde_json::json!({
            "lab_contract_version": 7,
            "roots": [{
                "remote_path": home.path().join("runner/workspace"),
                "object_cache": home.path().join("objects.git"),
                "manifest": {"entry_count": 0, "entries": []},
                "content_hash": "unused-by-plan"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    // Adjacent successes, both anonymous and in a known session, must not
    // become retries merely because their command paths match.
    for caller in [None, Some("materialization-e2e")] {
        for _ in 0..2 {
            let output = run(
                &[
                    "runner",
                    "workspace",
                    "materialization",
                    "--input",
                    input.to_str().unwrap(),
                    "--response",
                    response.to_str().unwrap(),
                ],
                caller,
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(response.exists());
        }
    }
    // A real repeated refusal retains a durable link to the prior failure.
    for _ in 0..2 {
        assert!(!run(
            &["runs", "friction", "--last", "invalid"],
            Some("retry-e2e")
        )
        .status
        .success());
    }
    let output = run(&["runs", "friction", "--last", "1d"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let data = &report["data"]["payload"];
    assert_eq!(
        data["immediate_reruns"],
        serde_json::json!([
            {"key": "runs friction", "count": 1}
        ])
    );
    let links = data["rerun_failures"].as_array().unwrap();
    assert_eq!(links.len(), 1);
    let failure = links[0]["failure_run_id"].as_str().unwrap();
    let shown = run(&["runs", "show", failure, "--format", "json"], None);
    assert!(shown.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert!(shown.to_string().contains("validation.invalid_argument"));
}
