use super::super::*;
use types::RunnerDoctorStatus;

#[test]
fn shell_path_expr_expands_runner_home_relative_paths() {
    assert_eq!(
        common::shell_path_expr("~/.cache/homeboy/source"),
        "\"${HOME}\"/'.cache/homeboy/source'"
    );
    assert_eq!(common::shell_path_expr("~"), "\"${HOME}\"");
    assert_eq!(common::shell_path_expr("/tmp/source"), "'/tmp/source'");
}

#[test]
fn normalizes_required_tools_before_preflight_checks() {
    assert_eq!(
        normalized_required_tools(&[
            " zip ".to_string(),
            "".to_string(),
            "tar".to_string(),
            "zip".to_string(),
        ]),
        vec!["tar".to_string(), "zip".to_string()]
    );
}

#[test]
fn required_tool_check_errors_with_actionable_remediation() {
    let check = checks::required_tool_check(
        "zip",
        &types::ToolProbe {
            available: false,
            path: None,
            version: None,
            error: Some("not found on PATH".to_string()),
        },
    );

    assert_eq!(check.id, "tool.required.zip");
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert!(check.message.contains("zip"));
    assert_eq!(
        check.details.get("command").map(String::as_str),
        Some("zip")
    );
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("Install zip on the runner")));
}

#[test]
fn required_homeboy_tools_capture_versions() {
    assert_eq!(
        probes::required_tool_version_args("homeboy"),
        &["--version"]
    );
    assert_eq!(
        probes::required_tool_version_args("/home/user/.cargo/bin/homeboy"),
        &["--version"]
    );
    assert!(probes::required_tool_version_args("git").is_empty());
}

#[test]
fn local_doctor_honors_required_tool_errors() {
    let (report, exit_code) = run_with_options(
        "local",
        RunnerDoctorOptions {
            path: None,
            extensions: Vec::new(),
            required_tools: vec!["homeboy-definitely-missing-tool".to_string()],
            ..Default::default()
        },
    )
    .expect("local doctor report");

    assert_eq!(exit_code, 0);
    assert_eq!(report.status, RunnerDoctorStatus::Error);
    assert!(report.checks.iter().any(|check| {
        check.id == "tool.required.homeboy-definitely-missing-tool"
            && check.status == RunnerDoctorStatus::Error
    }));
}
