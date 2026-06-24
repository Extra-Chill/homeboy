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

    // Guard: the fixture must actually present a truncation condition, otherwise
    // the function would short-circuit on the `!truncated` early return and this
    // test would never reach the structured-output branch it is named for.
    let capture = output
        .capture
        .as_ref()
        .expect("fixture provides capture metadata");
    assert!(
        capture.stdout.truncated || capture.stderr.truncated,
        "fixture must model a truncated stream so the structured-output branch is exercised"
    );

    // Same truncated output WITHOUT a structured output file is rejected, proving
    // that it is specifically the structured output file that flips the decision.
    let rejected = ensure_lab_offload_streams_not_truncated(&output, false);
    let err = rejected.expect_err("truncated streams without structured output must be rejected");
    assert_eq!(err.code, ErrorCode::InternalUnexpected);

    // With a structured output file present, the real production decision allows
    // the truncated streams (the complete result is preserved out-of-band).
    let allowed = ensure_lab_offload_streams_not_truncated(&output, true);
    assert!(
        allowed.is_ok(),
        "structured output file must permit truncated streams, got: {allowed:?}"
    );
}

#[test]
fn lab_artifact_dir_is_a_sibling_outside_the_checkout() {
    let checkout = "/srv/runner/workspaces/homeboy-core";
    let artifact_dir = remote_lab_artifact_dir(checkout);

    // The artifact directory must NOT live inside the synced git checkout,
    // otherwise its structured output would dirty the workspace (#6219).
    assert_eq!(
        artifact_dir,
        "/srv/runner/workspaces/homeboy-core-homeboy-artifacts"
    );
    assert!(
        !artifact_dir.starts_with(&format!("{checkout}/")),
        "artifact dir `{artifact_dir}` must not be nested inside checkout `{checkout}`"
    );
}

#[test]
fn lab_artifact_dir_ignores_trailing_slash_on_checkout() {
    assert_eq!(
        remote_lab_artifact_dir("/srv/runner/workspaces/homeboy-core/"),
        "/srv/runner/workspaces/homeboy-core-homeboy-artifacts"
    );
}

#[test]
fn lab_structured_output_file_is_written_outside_the_checkout() {
    let checkout = "/srv/runner/workspaces/homeboy-core";
    let output_file = remote_lab_output_file(checkout);

    // Structured output goes into the Homeboy-owned sibling artifact directory,
    // never directly into the synced checkout root.
    assert!(
        output_file.starts_with(&format!("{checkout}-homeboy-artifacts/")),
        "structured output `{output_file}` must live in the sibling artifact dir"
    );
    assert!(
        !output_file.starts_with(&format!("{checkout}/")),
        "structured output `{output_file}` must not dirty the checkout `{checkout}`"
    );
    assert!(output_file.ends_with(".json"));
    assert!(output_file.contains("homeboy-lab-structured-output-"));
}

#[test]
fn lab_cannot_proceed_error_names_runner_workspace_ref_dependency_and_fix_command() {
    // A bare dependency-resolution failure as it surfaces today, before
    // orchestration context is woven in.
    let bare = Error::validation_invalid_argument(
        "dependency",
        "Could not resolve dependency checkout",
        Some("sample-dependency".to_string()),
        None,
    );

    let mut context = LabOrchestrationContext::for_runner_workspace(
        "homeboy-lab",
        "/Users/dev/Developer/sample-project",
    )
    .with_ref_base(Some("origin/main".to_string()));
    context.dependency = Some("sample-dependency".to_string());

    let enriched = enrich_lab_cannot_proceed_error(bare, &context);

    // Structured context for machine consumers.
    let ctx = &enriched.details["lab_orchestration_context"];
    assert_eq!(ctx["runner_id"], "homeboy-lab");
    assert_eq!(ctx["workspace_path"], "/Users/dev/Developer/sample-project");
    assert_eq!(ctx["ref_base"], "origin/main");
    assert_eq!(ctx["dependency"], "sample-dependency");

    // Operator-facing hints name each known fact plus a Homeboy fix command.
    let hints = enriched
        .hints
        .iter()
        .map(|hint| hint.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        hints.iter().any(|hint| hint.contains("homeboy-lab")),
        "missing selected runner: {hints:?}"
    );
    assert!(
        hints
            .iter()
            .any(|hint| hint.contains("/Users/dev/Developer/sample-project")),
        "missing workspace path: {hints:?}"
    );
    assert!(
        hints.iter().any(|hint| hint.contains("origin/main")),
        "missing ref/base: {hints:?}"
    );
    assert!(
        hints.iter().any(|hint| hint.contains("sample-dependency")),
        "missing dependency: {hints:?}"
    );
    assert!(
        hints
            .iter()
            .any(|hint| hint.contains("homeboy runner status homeboy-lab")
                && hint.contains("homeboy deps install")),
        "missing concrete Homeboy fix command: {hints:?}"
    );
}

#[test]
fn lab_cannot_proceed_enrichment_is_idempotent() {
    let bare = Error::validation_invalid_argument(
        "changed_since",
        "Lab offload cannot resolve the requested --changed-since base before dispatch",
        Some("origin/main".to_string()),
        None,
    );
    let context = LabOrchestrationContext::for_runner_workspace(
        "homeboy-lab",
        "/Users/dev/Developer/sample-project",
    )
    .with_ref_base(Some("origin/main".to_string()));

    let once = enrich_lab_cannot_proceed_error(bare, &context);
    let hints_after_first = once.hints.len();
    let twice = enrich_lab_cannot_proceed_error(once, &context);

    // Re-enriching the same error must not duplicate context or fix hints.
    assert_eq!(twice.hints.len(), hints_after_first);
    assert_eq!(
        twice.details["lab_orchestration_context"]["runner_id"],
        "homeboy-lab"
    );
}
