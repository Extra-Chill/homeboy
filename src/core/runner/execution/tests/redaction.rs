use super::*;
use serde_json::json;

#[test]
fn runner_exec_redacts_env_diagnostic_assignments() {
    let env = HashMap::from([(
        "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
        "preview-token-secret".to_string(),
    )]);

    let (stdout, stderr) = redact_runner_exec_streams(
        "HOMEBOY_PREVIEW_TUNNEL_TOKEN=preview-token-secret\nSAFE=value\n".to_string(),
        "token=preview-token-secret\n".to_string(),
        &env,
        &[],
    );

    assert_eq!(
        stdout,
        "HOMEBOY_PREVIEW_TUNNEL_TOKEN=[REDACTED]\nSAFE=value\n"
    );
    assert_eq!(stderr, "token=[REDACTED]\n");
}

#[test]
fn runner_exec_redacts_bare_secret_values() {
    let env = HashMap::from([(
        "OPENAI_API_KEY".to_string(),
        "sk-test-secret-value".to_string(),
    )]);

    let (stdout, stderr) = redact_runner_exec_streams(
        "sk-test-secret-value\n".to_string(),
        "failed with sk-test-secret-value".to_string(),
        &env,
        &[],
    );

    assert_eq!(stdout, "[REDACTED]\n");
    assert_eq!(stderr, "failed with [REDACTED]");
}

#[test]
fn runner_exec_redacts_daemon_job_events() {
    let env = HashMap::from([(
        "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
        "preview-token-secret".to_string(),
    )]);
    let events = vec![crate::core::api_jobs::JobEvent {
        sequence: 1,
        job_id: uuid::Uuid::new_v4(),
        kind: crate::core::api_jobs::JobEventKind::Result,
        timestamp_ms: 1,
        message: Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN=preview-token-secret".to_string()),
        data: Some(json!({
            "stdout": "preview-token-secret",
            "stderr": "token=preview-token-secret",
        })),
    }];

    let redacted = redact_runner_job_events(&events, &env, &[]);

    assert_eq!(
        redacted[0].message.as_deref(),
        Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN=[REDACTED]")
    );
    assert_eq!(redacted[0].data.as_ref().unwrap()["stdout"], "[REDACTED]");
    assert_eq!(
        redacted[0].data.as_ref().unwrap()["stderr"],
        "token=[REDACTED]"
    );
}

#[test]
fn runner_exec_failure_error_promotes_homeboy_stdout_error() {
    let output = failed_runner_exec_output(
        r#"{"success":false,"error":{"code":"validation.invalid_argument","message":"Invalid argument 'source': Path does not exist: /Users/user/Developer/homeboy-extensions/wordpress","details":{"field":"source"}}}"#,
        "",
    );

    let err = runner_exec_failure_error(&output).expect("runner failure error");

    assert_eq!(err.code, ErrorCode::RemoteCommandFailed);
    assert!(err.message.contains("Path does not exist"));
    assert_eq!(
        err.details["runner_error"]["code"].as_str(),
        Some("validation.invalid_argument")
    );
    assert_eq!(err.details["runner_id"].as_str(), Some("lab"));
    assert_eq!(err.details["job_id"].as_str(), Some("job-123"));
    assert_eq!(
        err.details["remote_cwd"].as_str(),
        Some("/srv/homeboy/project")
    );
    assert_eq!(err.details["exit_code"].as_i64(), Some(2));
    assert_eq!(err.details["failure_context"]["contract_field"], "source");
    assert_eq!(
        err.details["execution"]["stdout"].as_str(),
        Some(output.stdout.as_str())
    );
}

#[test]
fn runner_exec_failure_error_surfaces_canonical_failure_context() {
    let mut output = failed_runner_exec_output(
        r#"{"success":false,"error":{"code":"validation.invalid_argument","message":"Missing required field: cwd","details":{"field":"cwd"}}}"#,
        "",
    );
    output.mirror_run_id = Some("runner-exec-lab-job-123".to_string());

    let err = runner_exec_failure_error(&output).expect("runner failure error");

    assert_eq!(
        err.details["failure_context"]["schema"].as_str(),
        Some("homeboy/runner-exec-failure-context/v1")
    );
    assert_eq!(
        err.details["failure_context"]["job_id"].as_str(),
        Some("job-123")
    );
    assert_eq!(
        err.details["failure_context"]["persisted_run_id"].as_str(),
        Some("runner-exec-lab-job-123")
    );
    assert_eq!(
        err.details["failure_context"]["contract_field"].as_str(),
        Some("cwd")
    );
    assert_eq!(
        err.details["failure_context"]["reason"].as_str(),
        Some("Missing required field: cwd")
    );
    let hints = err
        .hints
        .iter()
        .map(|hint| hint.message.as_str())
        .collect::<Vec<_>>();
    assert!(hints
        .iter()
        .any(|hint| hint.contains("runner job: `job-123`")));
    assert!(hints
        .iter()
        .any(|hint| hint.contains("persisted run: `runner-exec-lab-job-123`")));
    assert!(hints
        .iter()
        .any(|hint| hint.contains("contract field: `cwd`")));
}

#[test]
fn runner_exec_failure_error_keeps_rig_not_found_without_contract_field() {
    let output = failed_runner_exec_output(
        "",
        r#"{"success":false,"error":{"code":"rig.not_found","message":"Rig not found","details":{"rig":"missing"}}}"#,
    );

    let err = runner_exec_failure_error(&output).expect("runner failure error");

    assert_eq!(
        err.details["failure_context"]["error_code"].as_str(),
        Some("rig.not_found")
    );
    assert_eq!(
        err.details["failure_context"]["error_details"]["rig"].as_str(),
        Some("missing")
    );
    assert!(err.details["failure_context"]
        .get("contract_field")
        .is_none());
    let hints = err
        .hints
        .iter()
        .map(|hint| hint.message.as_str())
        .collect::<Vec<_>>();
    assert!(hints
        .iter()
        .any(|hint| hint.contains("structured error: `rig.not_found`")));
    assert!(hints
        .iter()
        .any(|hint| hint.contains("details: {\"rig\":\"missing\"}")));
    assert!(hints
        .iter()
        .any(|hint| hint.contains("homeboy runner exec lab -- homeboy rig list")));
    assert!(!hints
        .iter()
        .any(|hint| hint.contains("unknown contract field")));
}

#[test]
fn runner_exec_failure_error_promotes_homeboy_job_event_message_error() {
    let mut output = failed_runner_exec_output("", "generic stderr");
    output.job_events = Some(vec![crate::core::api_jobs::JobEvent {
        sequence: 1,
        job_id: uuid::Uuid::new_v4(),
        kind: crate::core::api_jobs::JobEventKind::Error,
        timestamp_ms: 1,
        message: Some(
            r#"runner emitted: {"success":false,"error":{"code":"extension.not_found","message":"Extension not found: wordpress"}}"#
                .to_string(),
        ),
        data: None,
    }]);

    let err = runner_exec_failure_error(&output).expect("runner failure error");

    assert!(err.message.contains("Extension not found: wordpress"));
    assert_eq!(
        err.details["runner_error"]["code"].as_str(),
        Some("extension.not_found")
    );
    assert!(err.details["execution"]["job_events"].is_array());
}
