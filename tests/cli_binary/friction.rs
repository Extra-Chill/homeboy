use std::process::Command;

#[test]
fn friction_reports_real_success_and_validation_refusal() {
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_homeboy"))
            .args(args)
            .env("HOME", home.path())
            .env("HOMEBOY_DATA_DIR", home.path().join("data"))
            .env("HOMEBOY_CALLER_SESSION", "friction-e2e")
            .output()
            .unwrap()
    };
    let success = run(&["runs", "list"]);
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    let refusal = run(&["runs", "friction", "--last", "not-a-duration"]);
    assert!(!refusal.status.success());
    let report = run(&["runs", "friction", "--last", "7d"]);
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    let data = &report["data"]["payload"];
    assert_eq!(data["invocation_count"], 2, "{report}");
    let invocations = data["invocations"].as_array().unwrap();
    assert!(invocations.iter().any(
        |r| r["invocation"]["command_path"] == "runs list" && r["invocation"]["exit_code"] == 0
    ));
    assert!(invocations
        .iter()
        .any(|r| r["invocation"]["command_path"] == "runs friction"
            && r["invocation"]["error_code"] == "validation.invalid_argument"));
    assert_eq!(
        data["top_refusal_codes"][0]["key"],
        "validation.invalid_argument"
    );
    assert!(!report.to_string().contains("not-a-duration"));
}
