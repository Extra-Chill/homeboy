use super::*;

#[test]
fn apply_patch_step_accepts_noop_mutation_return() {
    let plan = base_lab_plan(Some(&portable_lab_command("refactor")));

    let plan = with_lab_apply_patch_step(plan, None);

    let step = plan
        .steps
        .iter()
        .find(|step| step.id == "lab.apply_patch")
        .expect("apply patch step");
    assert_eq!(step.status, PlanStepStatus::Success);
    assert_eq!(step.inputs["apply"]["applied"], serde_json::json!(false));
    assert_eq!(
        step.inputs["apply"]["reason"],
        serde_json::json!("no_patch")
    );
}

#[test]
fn lab_offload_rejects_truncated_runner_stdout() {
    let exec_output = RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: "lab-default".to_string(),
        dry_run: false,
        mode: RunnerExecMode::Daemon,
        argv: vec!["homeboy".to_string(), "agent-task".to_string()],
        remote_cwd: "/srv/homeboy/_lab_workspaces/sample-plugin-code".to_string(),
        exit_code: 0,
        stdout: "tail-only-json-fragment".to_string(),
        stderr: String::new(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: Some("job-123".to_string()),
        job_events: None,
        mirror_run_id: Some("runner-exec-lab-default-job-123".to_string()),
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        metrics: None,
        capture: Some(CommandCaptureMetadata {
            stdout: CaptureMetadata {
                bytes_seen: 4_500_000,
                bytes_retained: 4 * 1024 * 1024,
                byte_limit: 4 * 1024 * 1024,
                truncated: true,
            },
            stderr: CaptureMetadata::default(),
        }),
        runner_result: None,
        handoff: None,
        diagnostics: None,
    };

    let err = ensure_lab_offload_streams_not_truncated(&exec_output, false)
        .expect_err("truncated stdout is rejected");

    assert_eq!(err.code.as_str(), "internal.unexpected");
    assert!(err.message.contains("output exceeded"));
    assert_eq!(err.details["runner_id"], "lab-default");
    assert_eq!(err.details["job_id"], "job-123");
    assert_eq!(err.details["capture"]["stdout"]["truncated"], true);
}

#[test]
fn lab_offload_failure_summary_uses_runner_failure_context() {
    let exec_output = RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: "lab-default".to_string(),
        dry_run: false,
        mode: RunnerExecMode::Daemon,
        argv: vec!["homeboy".to_string(), "test".to_string()],
        remote_cwd: "/srv/homeboy/_lab_workspaces/sample-plugin-code".to_string(),
        exit_code: 2,
        stdout: String::new(),
        stderr: r#"{"success":false,"error":{"code":"validation.invalid_argument","message":"Missing required field: cwd","details":{"field":"cwd"}}}"#.to_string(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: Some("job-123".to_string()),
        job_events: None,
        mirror_run_id: Some("runner-exec-lab-default-job-123".to_string()),
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        metrics: None,
        capture: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    };
    let mut stderr = String::new();

    append_runner_failure_context_summary(&mut stderr, &exec_output);

    assert!(stderr.contains("command `homeboy test`"));
    assert!(stderr.contains("runner job `job-123`"));
    assert!(stderr.contains("persisted run `runner-exec-lab-default-job-123`"));
    assert!(stderr.contains("contract field `cwd`"));
    assert!(stderr.contains("Missing required field: cwd"));
}

#[test]
fn missing_mutation_patch_error_points_to_runner_evidence_and_retry() {
    let exec_output = RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: "lab-default".to_string(),
        dry_run: false,
        mode: RunnerExecMode::Daemon,
        argv: vec![
            "homeboy".to_string(),
            "refactor".to_string(),
            "--from".to_string(),
            "lint".to_string(),
            "--write".to_string(),
            "sample-plugin-code".to_string(),
        ],
        remote_cwd: "/srv/homeboy/_lab_workspaces/sample-plugin-code".to_string(),
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: Some("job-123".to_string()),
        job_events: None,
        mirror_run_id: Some("runner-exec-lab-default-job-123".to_string()),
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        metrics: None,
        capture: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    };

    let err = missing_mutation_patch_error(
        &[
            "homeboy".to_string(),
            "refactor".to_string(),
            "--from".to_string(),
            "lint".to_string(),
            "--write".to_string(),
            "sample-plugin-code".to_string(),
        ],
        Some("--write"),
        &exec_output,
    );

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("returned no source-tree patch"));
    assert_eq!(err.details["runner_id"], "lab-default");
    assert_eq!(err.details["job_id"], "job-123");
    assert_eq!(
        err.details["mirror_run_id"],
        "runner-exec-lab-default-job-123"
    );
    let hints = err
        .hints
        .iter()
        .map(|hint| hint.message.as_str())
        .collect::<Vec<_>>();
    assert!(hints
        .iter()
        .any(|hint| hint.contains("homeboy runs show runner-exec-lab-default-job-123")));
    assert!(hints
        .iter()
        .any(|hint| hint.contains("homeboy runs artifacts runner-exec-lab-default-job-123")));
    assert!(hints
        .iter()
        .any(|hint| hint.contains("homeboy refactor --from lint --write sample-plugin-code")));
}

#[test]
fn default_runner_missing_capabilities_fails_without_local_fallback_opt_in() {
    let plan = base_lab_plan(Some(&portable_lab_command("trace")));
    let selection = LabRunnerSelection {
        runner_id: "homeboy-lab".to_string(),
        source: LabRunnerSelectionSource::Default,
        mode: RunnerTunnelMode::Reverse,
    };
    let status = reverse_status("homeboy-lab");

    let result = automatic_capability_fallback_or_error(
        plan,
        &selection,
        &status,
        "Runner 'homeboy-lab' is missing required capability parity for `trace`: tools: playwright."
            .to_string(),
        vec!["Install Playwright and browser binaries on the runner.".to_string()],
        false,
        false,
    );

    let Err(err) = result else {
        panic!("expected selected default runner to fail fast");
    };
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("missing required capability parity"));
    assert!(err.message.contains("playwright"));
    assert_eq!(err.details["id"], "homeboy-lab");
    let tried = err.details["tried"].as_array().expect("tried");
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("--allow-local-fallback"))));
}

#[test]
fn default_runner_missing_capabilities_can_fallback_with_explicit_opt_in() {
    let plan = base_lab_plan(Some(&portable_lab_command("trace")));
    let selection = LabRunnerSelection {
        runner_id: "homeboy-lab".to_string(),
        source: LabRunnerSelectionSource::Default,
        mode: RunnerTunnelMode::Reverse,
    };
    let status = reverse_status("homeboy-lab");

    let outcome = automatic_capability_fallback_or_error(
        plan,
        &selection,
        &status,
        "Runner 'homeboy-lab' is missing required capability parity for `trace`: tools: playwright."
            .to_string(),
        Vec::new(),
        true,
        false,
    )
    .expect("explicit fallback opt-in should allow local run");

    let LabOffloadOutcome::RunLocal {
        messages, metadata, ..
    } = outcome
    else {
        panic!("expected local fallback");
    };
    assert!(messages[0].contains("running locally"));
    assert_eq!(metadata.expect("metadata")["status"], "fallback");
}

#[test]
fn plan_records_skipped_auto_offload() {
    let outcome = execute_lab_offload(LabOffloadRequest {
        command: Some(portable_lab_command("test")),
        normalized_args: &["homeboy".to_string(), "test".to_string()],
        explicit_runner: None,
        force_hot: true,
        local_policy: LabLocalExecutionPolicy::from_flags(true, false, false),
        allow_dirty_lab_workspace: false,
        capture_patch: false,
        mutation_flag: None,
        detach_after_handoff: false,
        output_file_requested: false,
        local_output_file: None,
    })
    .expect("outcome");

    let LabOffloadOutcome::RunLocal { plan, metadata, .. } = outcome else {
        panic!("force-hot should run locally");
    };
    assert_eq!(plan.kind, PlanKind::LabOffload);
    assert_eq!(plan.steps[0].id, "lab.select_runner");
    assert_eq!(plan.steps[0].status, PlanStepStatus::Skipped);
    assert_eq!(metadata.expect("metadata")["status"], "skipped");
}

#[test]
fn lab_only_refuses_local_execution_without_lab_contract() {
    let outcome = execute_lab_offload(LabOffloadRequest {
        command: None,
        normalized_args: &["homeboy".to_string(), "status".to_string()],
        explicit_runner: None,
        force_hot: false,
        local_policy: LabLocalExecutionPolicy::from_flags(false, false, true),
        allow_dirty_lab_workspace: false,
        capture_patch: false,
        mutation_flag: None,
        detach_after_handoff: false,
        output_file_requested: false,
        local_output_file: None,
    });

    let Err(err) = outcome else {
        panic!("lab-only should refuse local execution");
    };
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("Lab-only execution refused"));
}

#[test]
fn build_runner_error_gives_managed_runner_replacement() {
    let outcome = execute_lab_offload(LabOffloadRequest {
        command: None,
        normalized_args: &[
            "homeboy".to_string(),
            "build".to_string(),
            "homeboy".to_string(),
        ],
        explicit_runner: Some("homeboy-lab"),
        force_hot: false,
        local_policy: LabLocalExecutionPolicy::default(),
        allow_dirty_lab_workspace: false,
        capture_patch: false,
        mutation_flag: None,
        detach_after_handoff: false,
        output_file_requested: false,
        local_output_file: None,
    });

    let Err(err) = outcome else {
        panic!("build --runner should fail before local execution");
    };
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err
        .message
        .contains("homeboy build is not Lab-portable yet"));
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried
        .iter()
        .any(|hint| hint.as_str().is_some_and(|hint| hint.contains(
            "homeboy runner workspace sync homeboy-lab --path <local-worktree> --mode snapshot"
        ))));
    assert!(tried
        .iter()
        .any(|hint| hint.as_str().is_some_and(|hint| hint.contains(
            "homeboy runner exec homeboy-lab --cwd <runner_path> -- homeboy build <component>"
        ))));
}

#[test]
fn build_lab_only_error_gives_managed_runner_replacement() {
    let outcome = execute_lab_offload(LabOffloadRequest {
        command: None,
        normalized_args: &[
            "homeboy".to_string(),
            "build".to_string(),
            "homeboy".to_string(),
        ],
        explicit_runner: None,
        force_hot: false,
        local_policy: LabLocalExecutionPolicy::from_flags(false, false, true),
        allow_dirty_lab_workspace: false,
        capture_patch: false,
        mutation_flag: None,
        detach_after_handoff: false,
        output_file_requested: false,
        local_output_file: None,
    });

    let Err(err) = outcome else {
        panic!("build --lab-only should fail before local execution");
    };
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err
        .message
        .contains("homeboy build is not Lab-portable yet"));
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried
        .iter()
        .any(|hint| hint.as_str().is_some_and(|hint| hint.contains(
            "homeboy runner workspace sync <runner-id> --path <local-worktree> --mode snapshot"
        ))));
    assert!(tried
        .iter()
        .any(|hint| hint.as_str().is_some_and(|hint| hint.contains(
            "homeboy runner exec <runner-id> --cwd <runner_path> -- homeboy build <component>"
        ))));
}

#[test]
fn unsupported_runner_error_guides_tunnel_service_inspection() {
    let outcome = execute_lab_offload(LabOffloadRequest {
        command: None,
        normalized_args: &[
            "homeboy".to_string(),
            "tunnel".to_string(),
            "service".to_string(),
            "status".to_string(),
            "wpcom-ai-manual-held".to_string(),
        ],
        explicit_runner: Some("homeboy-lab"),
        force_hot: false,
        local_policy: LabLocalExecutionPolicy::default(),
        allow_dirty_lab_workspace: false,
        capture_patch: false,
        mutation_flag: None,
        detach_after_handoff: false,
        output_file_requested: false,
        local_output_file: None,
    });

    let Err(err) = outcome else {
        panic!("unsupported --runner command should fail");
    };
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("homeboy runner exec homeboy-lab"))));
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("tunnel service status"))));
}

#[test]
fn lab_stream_truncation_fails_without_structured_output_file() {
    let output = truncated_runner_exec_output();

    let err = ensure_lab_offload_streams_not_truncated(&output, false)
        .expect_err("truncated streams without structured output should fail");

    assert_eq!(err.code, ErrorCode::InternalUnexpected);
    assert!(err.message.contains("retained stream limit"));
}

#[test]
fn lab_stream_truncation_is_allowed_with_structured_output_file() {
    let output = truncated_runner_exec_output();

    ensure_lab_offload_streams_not_truncated(&output, true)
        .expect("structured output file preserves complete command result");
}
