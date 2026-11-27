use super::super::*;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn ensure_agent_task_dispatch_run_id_keeps_existing_ids_and_injects_before_options() {
    // (args, preferred isolation token, expected run id, args unchanged)
    let cases = [
        (
            args(&[
                "homeboy",
                "agent-task",
                "cook",
                "--run-id",
                "cook-run",
                "--repo",
                "homeboy",
            ]),
            None,
            "cook-run",
        ),
        // An explicit --run-id always wins over the preferred isolation token.
        (
            args(&["homeboy", "agent-task", "cook", "--run-id", "explicit-run"]),
            Some("iso-token"),
            "explicit-run",
        ),
        // A materialized run-plan already carries its durable run id.
        (
            args(&[
                "homeboy",
                "agent-task",
                "run-plan",
                "--plan",
                "@/runner/retry-plan.json",
                "--record-run-id",
                "retry-run",
            ]),
            None,
            "retry-run",
        ),
    ];
    for (input, preferred, expected) in cases {
        let (out, run_id) =
            ensure_agent_task_dispatch_run_id_with(&input, preferred).expect("agent task args");
        assert_eq!(run_id, expected);
        assert_eq!(out, input);
    }

    let unset = args(&["homeboy", "agent-task", "cook", "--repo", "homeboy"]);
    for (preferred, generated) in [(Some("iso-token"), false), (None, true)] {
        let (out, run_id) =
            ensure_agent_task_dispatch_run_id_with(&unset, preferred).expect("agent task args");
        if generated {
            assert!(run_id.starts_with("agent-task-"), "{run_id}");
        } else {
            assert_eq!(run_id, "iso-token");
        }
        // `--run-id` is injected right after the `agent-task <action>` prefix,
        // ahead of the dispatch options.
        assert_eq!(
            out,
            args(&[
                "homeboy",
                "agent-task",
                "cook",
                "--run-id",
                &run_id,
                "--repo",
                "homeboy"
            ])
        );
    }

    assert!(ensure_agent_task_dispatch_run_id(&args(&[
        "homeboy",
        "agent-task",
        "status",
        "run-1"
    ]))
    .is_none());
}

#[test]
fn lab_cook_uses_one_first_attempt_identity_across_the_handoff() {
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--run-id".to_string(),
        "cook-7970".to_string(),
    ];

    let (out, lifecycle_run_id) =
        ensure_agent_task_lifecycle_identity_with(&args, None, None).expect("cook identity");

    assert_eq!(out[3], "--attempt-run-id");
    assert_eq!(out[4], lifecycle_run_id);
    assert_eq!(out[5], "--run-id");
    assert_eq!(out[6], "cook-7970");
    assert!(lifecycle_run_id.starts_with("cook-7970-attempt-1-"));

    let (staged_args, staged_lifecycle_run_id) =
        ensure_agent_task_lifecycle_identity_with(&out, None, None).expect("staged cook identity");

    assert_eq!(staged_args, out);
    assert_eq!(staged_lifecycle_run_id, lifecycle_run_id);
}

#[test]
fn lab_cook_staging_preserves_generated_durable_lifecycle_identity() {
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--repo".to_string(),
        "homeboy".to_string(),
    ];

    let (pre_acceptance_args, pre_acceptance_run_id) =
        ensure_agent_task_lifecycle_identity_with(&args, Some("cook-8005"), None)
            .expect("pre-acceptance cook identity");
    let (staged_args, staged_run_id) =
        ensure_agent_task_lifecycle_identity_with(&pre_acceptance_args, Some("other-token"), None)
            .expect("staged cook identity");

    assert!(pre_acceptance_run_id.starts_with("cook-8005-attempt-1-"));
    assert_eq!(staged_run_id, pre_acceptance_run_id);
    assert_eq!(staged_args, pre_acceptance_args);
    assert_eq!(
        agent_task_dispatch_requested_run_id(&staged_args),
        Some("cook-8005".to_string())
    );
}

#[test]
fn lab_cook_preserves_explicit_attempt_identity_for_drift_detection() {
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--attempt-run-id".to_string(),
        "unexpected-attempt".to_string(),
        "--run-id".to_string(),
        "cook-8009".to_string(),
    ];

    let (out, lifecycle_run_id) = ensure_agent_task_lifecycle_identity_with(
        &args,
        Some("cook-8009"),
        Some("cook-8009-attempt-1-canonical"),
    )
    .expect("cook identity");

    assert_eq!(out, args);
    assert_eq!(lifecycle_run_id, "unexpected-attempt");
}

#[test]
fn lifecycle_identity_preserves_materialized_run_plan_id() {
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        "@/runner/retry-plan.json".to_string(),
        "--record-run-id".to_string(),
        "cook-8332-attempt-1".to_string(),
    ];

    let (out, run_id) = ensure_agent_task_lifecycle_identity_with(&args, None, None)
        .expect("materialized run-plan has a lifecycle identity");

    assert_eq!(out, args);
    assert_eq!(run_id, "cook-8332-attempt-1");
}

#[test]
fn dispatch_run_isolation_token_reuses_or_generates_only_for_dispatch_commands() {
    assert_eq!(
        agent_task_dispatch_run_isolation_token(&args(&[
            "homeboy",
            "agent-task",
            "dispatch",
            "--run-id",
            "explicit-run",
        ])),
        Some("explicit-run".to_string())
    );
    let token = agent_task_dispatch_run_isolation_token(&args(&[
        "homeboy",
        "agent-task",
        "cook",
        "--repo",
        "homeboy",
    ]))
    .expect("token");
    assert!(token.starts_with("agent-task-"));
    assert!(agent_task_dispatch_run_isolation_token(&args(&[
        "homeboy",
        "agent-task",
        "status",
        "run-1",
    ]))
    .is_none());
}

#[test]
fn materializes_inline_agent_task_cook_tasks_json() {
    let prompt = "Cook sensitive implementation details";
    let tasks = serde_json::json!([{ "prompt": prompt }]).to_string();
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--tasks".to_string(),
        tasks.clone(),
        "--concurrency".to_string(),
        "4".to_string(),
    ];

    let (rewritten, entry) = materialize_inline_agent_task_tasks_arg_with(&args, |spec| {
        assert_eq!(spec, tasks);
        Ok(Some(fake_synced_file(
            "@/remote/input/agent-task-tasks.json",
            "agent_task_tasks_remapped",
        )))
    })
    .expect("rewrite tasks arg");

    assert_eq!(
        rewritten,
        vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--tasks".to_string(),
            "@/remote/input/agent-task-tasks.json".to_string(),
            "--concurrency".to_string(),
            "4".to_string(),
        ]
    );
    assert!(!rewritten.join(" ").contains(prompt));
    assert_eq!(entry.expect("mapping entry").remote_path(), "/remote/input");
}

#[test]
fn materializes_inline_cook_attempt_plan_only_for_runner_execution() {
    let plan = r#"{"schema":"homeboy/agent-task-plan/v1","plan_id":"controller-plan","tasks":[]}"#;
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--attempt-plan".to_string(),
        plan.to_string(),
    ];

    let (rewritten, entry) = materialize_inline_agent_task_tasks_arg_with(&args, |spec| {
        assert_eq!(spec, plan);
        Ok(Some(fake_synced_file(
            "@/remote/input/agent-task-attempt-plan.json",
            "agent_task_attempt_plan_remapped",
        )))
    })
    .expect("rewrite attempt plan");

    assert_eq!(rewritten[4], "@/remote/input/agent-task-attempt-plan.json");
    assert!(!rewritten.contains(&plan.to_string()));
    assert_eq!(entry.expect("mapping entry").remote_path(), "/remote/input");
}

#[test]
fn leaves_agent_task_tasks_file_specs_in_argv() {
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "dispatch".to_string(),
        "--tasks=@tasks.json".to_string(),
    ];

    let (rewritten, entry) = materialize_inline_agent_task_tasks_arg_with(&args, |spec| {
        assert_eq!(spec, "@tasks.json");
        Ok(None)
    })
    .expect("rewrite tasks arg");

    assert_eq!(rewritten, args);
    assert!(entry.is_none());
}

fn fake_synced_file(remote_spec: &str, role: &str) -> (String, LabWorkspaceMappingEntry) {
    let synced = crate::RunnerWorkspaceSyncOutput {
        variant: "workspace_sync",
        command: "runner.workspace.sync",
        runner_id: "lab".to_string(),
        local_path: "/local/input".to_string(),
        remote_path: "/remote/input".to_string(),
        materialization_plan: crate::RunnerWorkspaceMaterializationPlan::from_test_parts(
            "/remote",
            "/local/input",
            "input",
            "/remote/input",
            RunnerWorkspaceSyncMode::Snapshot,
            "snapshot",
        ),
        current_workspace: crate::RunnerWorkspaceCurrentSummary {
            local_path: "/local/input".to_string(),
            remote_path: "/remote/input".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            materialized: true,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
            synthetic_checkout_commit: None,
            synthetic_checkout_ref: None,
            synthetic_checkout_tree: None,
        },
        workspace_lease: crate::RunnerWorkspaceLease {
            runner_id: "lab".to_string(),
            local_path: "/local/input".to_string(),
            remote_path: "/remote/input".to_string(),
            sync_mode: "snapshot".to_string(),
            materialized: true,
            lifecycle_owner: crate::RunnerLifecycleOwner::Controller,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
        },
        resource_lifecycle: crate::workspace_resource_lifecycle(
            "lab",
            "/remote/input",
            None,
            homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
        ),
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        snapshot_identity: "snapshot".to_string(),
        content: None,
        workspace_ref: "workspace:00000000-0000-0000-0000-000000000001".to_string(),
        prepared_workspace_lease: None,
        counts: crate::ByteFileCounts {
            files: 1,
            bytes: 42,
        },
        excludes: Vec::new(),
        includes: Vec::new(),
        workspace_cleanliness: "clean".to_string(),
        validation_dependencies: Vec::new(),
    };
    (
        remote_spec.to_string(),
        workspace_mapping_entry(role, &synced),
    )
}

#[test]
fn pre_dispatch_failure_message_uses_structured_dependency_failure_envelope() {
    let output = r#"runtime setup log
{"schema":"homeboy/lab-dependency-failure/v1","dependency":{"id":"runtime-a","kind":"runtime component","path":"/remote/cache/runtime-a"},"message":"path missing","remediation":"refresh runtime cache"}
trailing log"#;

    let message = lab_pre_dispatch_failure_message(output).expect("message");

    assert!(message.contains("runtime component `/remote/cache/runtime-a`"));
    assert!(message.contains("path missing"));
    assert!(message.contains("refresh runtime cache"));
}
