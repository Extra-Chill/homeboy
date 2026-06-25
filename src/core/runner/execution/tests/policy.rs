use super::*;
use serde_json::json;
use std::time::Duration;

#[test]
fn test_required_extensions_for_command_reads_extension_flags() {
    let command = vec![
        "homeboy".to_string(),
        "lint".to_string(),
        "--extension".to_string(),
        "rust".to_string(),
        "--extension=fixture-build".to_string(),
    ];

    assert_eq!(
        required_extensions_for_command(&command, &["wordpress".to_string()]),
        vec![
            "wordpress".to_string(),
            "rust".to_string(),
            "fixture-build".to_string(),
        ]
    );
}

#[test]
fn test_runner_policy_denies_raw_ssh_exec_by_default() {
    let runner = ssh_runner();
    let options = RunnerExecOptions {
        cwd: Some("/srv/homeboy/project".to_string()),
        project_id: Some("extrachill".to_string()),
        allow_diagnostic_ssh: true,
        command: vec!["sh".to_string()],
        env: Default::default(),
        secret_env_names: Vec::new(),
        capture_patch: false,
        raw_exec: true,
        source_snapshot: None,
        capability_preflight: None,
        required_extensions: Vec::new(),
        require_paths: Vec::new(),
        runner_workload: None,
        detach_after_handoff: false,
        run_label: None,
    };

    let err = validate_runner_policy(&runner, "/srv/homeboy/project", policy_request(&options))
        .expect_err("deny raw exec");

    assert_eq!(err.code.as_str(), "runner.policy_denied");
    assert!(err.message.contains("raw exec is denied by default"));
}

#[test]
fn test_runner_policy_enforces_projects_commands_workspace_and_artifacts() {
    let mut runner = ssh_runner();
    runner.policy = RunnerPolicy {
        allow_raw_exec: Some(true),
        allowed_projects: vec!["extrachill".to_string()],
        allowed_commands: vec!["cargo".to_string()],
        workspace_roots: vec!["/srv/homeboy/extrachill".to_string()],
        artifact_policy: Some("deny".to_string()),
        ..Default::default()
    };

    let allowed = RunnerExecOptions {
        cwd: Some("/srv/homeboy/extrachill/homeboy".to_string()),
        project_id: Some("extrachill".to_string()),
        allow_diagnostic_ssh: true,
        command: vec!["cargo".to_string(), "test".to_string()],
        env: Default::default(),
        secret_env_names: Vec::new(),
        capture_patch: false,
        raw_exec: true,
        source_snapshot: None,
        capability_preflight: None,
        required_extensions: Vec::new(),
        require_paths: Vec::new(),
        runner_workload: None,
        detach_after_handoff: false,
        run_label: None,
    };
    validate_runner_policy(
        &runner,
        "/srv/homeboy/extrachill/homeboy",
        policy_request(&allowed),
    )
    .expect("allowed policy");

    let mut denied_project = allowed.clone();
    denied_project.project_id = Some("wire".to_string());
    assert_eq!(
        validate_runner_policy(
            &runner,
            "/srv/homeboy/extrachill/homeboy",
            policy_request(&denied_project),
        )
        .expect_err("deny project")
        .code
        .as_str(),
        "runner.policy_denied"
    );

    let mut denied_command = allowed.clone();
    denied_command.command = vec!["sh".to_string()];
    assert!(validate_runner_policy(
        &runner,
        "/srv/homeboy/extrachill/homeboy",
        policy_request(&denied_command)
    )
    .expect_err("deny command")
    .message
    .contains("command family 'sh'"));

    assert!(
        validate_runner_policy(&runner, "/srv/homeboy/other", policy_request(&allowed))
            .expect_err("deny workspace")
            .message
            .contains("workspace roots")
    );

    let mut denied_artifacts = allowed.clone();
    denied_artifacts.capture_patch = true;
    assert!(validate_runner_policy(
        &runner,
        "/srv/homeboy/extrachill/homeboy",
        policy_request(&denied_artifacts)
    )
    .expect_err("deny artifacts")
    .message
    .contains("artifact capture"));
}

#[test]
fn test_daemon_api_get_requires_connected_runner() {
    crate::test_support::with_isolated_home(|_| {
        super::super::super::create(
            r#"{"id":"lab-local","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create local runner");

        let err = daemon_api_get("lab-local", "/runs").expect_err("requires daemon");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("connected to a daemon"));
    });
}

#[test]
fn canonical_daemon_body_requires_nested_body() {
    let err = canonical_daemon_body(&json!({ "job": {} }), "daemon exec response")
        .expect_err("reject legacy direct data");
    assert!(err.message.contains("data.body"));
}

#[test]
fn canonical_daemon_body_returns_nested_body() {
    let data = json!({ "body": { "job": { "id": "job-1" } } });
    let body = canonical_daemon_body(&data, "daemon exec response").expect("body");
    assert_eq!(body["job"]["id"], "job-1");
}

#[test]
fn runner_exec_wait_timeout_defaults_to_controller_timeout_budget() {
    std::env::remove_var(RUNNER_EXEC_WAIT_TIMEOUT_ENV);
    assert_eq!(runner_exec_wait_timeout(), Duration::from_secs(20 * 60));
}
