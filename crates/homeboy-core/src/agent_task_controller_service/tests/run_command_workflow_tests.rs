use super::super::*;
use super::*;

#[test]
fn run_command_workflow_executes_deterministic_artifact_action() {
    with_isolated_home(|home| {
        let spec = AgentTaskRepoLoopSpec {
            schema: None,
            loop_id: "repo-loop-command".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
            metadata: Value::Null,
            entities: Vec::new(),
            agents: Vec::new(),
            tools: Vec::new(),
            abilities: Vec::new(),
            workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "deterministic-validation".to_string(),
                agent_id: None,
                prompt: None,
                tasks: vec!["Run deterministic validation.".to_string()],
                entity_ids: Vec::new(),
                fan_out: None,
                tools: Vec::new(),
                abilities: Vec::new(),
                artifacts: vec!["validation_result".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
                runtime_execution: json!({
                    "kind": "command",
                    "command": "/bin/sh",
                    "args": ["-c", "printf '%s\n' '{\"artifacts\":{\"validation_result\":{\"schema\":\"example/ValidationResult/v1\",\"artifact_url\":\"artifact://validation-result\"}}}' > \"$HOMEBOY_LOOP_ACTION_OUTPUT\""]
                }),
                inputs: Value::Null,
            }],
            artifacts: vec![AgentTaskRepoLoopSpecArtifact {
                artifact_id: "validation_result".to_string(),
                kind: "example/ValidationResult/v1".to_string(),
                description: None,
                required: true,
            }],
            artifact_graph: Vec::new(),
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            gate_bundles: Vec::new(),
            policy: None,
            phases: Vec::new(),
            actions: Vec::new(),
            initial_event: None,
        };

        let initialized = init_from_spec(ControllerFromSpecRequest { spec }).expect("init");
        match &initialized.actions[0].action {
            AgentTaskLoopPolicyAction::RunCommand { request, .. } => {
                assert_eq!(request["execution"]["kind"], "command");
            }
            other => panic!("expected run_command action, got {other:?}"),
        }

        let result = run_next(
            "repo-loop-command",
            CapturingExecutor::default(),
            &CapturingDispatchHook::default(),
        )
        .expect("run command action");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.status.as_deref(), Some("completed"));
        assert_eq!(
            result.value.execution.as_ref().unwrap()["result"]["artifacts"]["validation_result"]
                ["schema"],
            "example/ValidationResult/v1"
        );
        assert_eq!(result.value.controller.task_lineage.len(), 1);
        assert_eq!(
            result.value.controller.task_lineage[0].outputs["artifacts"]["validation_result"]
                ["artifact_url"],
            "artifact://validation-result"
        );
        let persisted_artifact = home
                .path()
                .join(".local/share/homeboy/artifacts/agent-task-loop-controller/repo-loop-command/action-1/validation_result.json");
        let persisted: Value = serde_json::from_str(
            &std::fs::read_to_string(&persisted_artifact).expect("persisted controller artifact"),
        )
        .expect("persisted artifact json");
        assert_eq!(persisted["schema"], "example/ValidationResult/v1");
    });
}

#[test]
fn run_command_workflow_inherits_dispatch_default_cwd() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    std::fs::write(repo.path().join("repo-marker"), "ok").expect("repo marker");
    with_isolated_home(|_| {
        let spec = AgentTaskRepoLoopSpec {
            schema: None,
            loop_id: "repo-loop-command-cwd".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
            metadata: json!({
                "dispatch_defaults": {
                    "cwd": repo.path().display().to_string()
                }
            }),
            entities: Vec::new(),
            agents: Vec::new(),
            tools: Vec::new(),
            abilities: Vec::new(),
            workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "deterministic-validation".to_string(),
                agent_id: None,
                prompt: None,
                tasks: vec!["Run deterministic validation.".to_string()],
                entity_ids: Vec::new(),
                fan_out: None,
                tools: Vec::new(),
                abilities: Vec::new(),
                artifacts: vec!["validation_result".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
                runtime_execution: json!({
                    "kind": "command",
                    "command": "/bin/sh",
                    "args": ["-c", "test -f repo-marker && printf '%s\n' '{\"artifacts\":{\"validation_result\":{\"schema\":\"example/ValidationResult/v1\"}}}' > \"$HOMEBOY_LOOP_ACTION_OUTPUT\""]
                }),
                inputs: Value::Null,
            }],
            artifacts: vec![AgentTaskRepoLoopSpecArtifact {
                artifact_id: "validation_result".to_string(),
                kind: "example/ValidationResult/v1".to_string(),
                description: None,
                required: true,
            }],
            artifact_graph: Vec::new(),
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            gate_bundles: Vec::new(),
            policy: None,
            phases: Vec::new(),
            actions: Vec::new(),
            initial_event: None,
        };

        let initialized = init_from_spec(ControllerFromSpecRequest { spec }).expect("init");
        match &initialized.actions[0].action {
            AgentTaskLoopPolicyAction::RunCommand { request, .. } => {
                assert_eq!(
                    request["execution"]["cwd"].as_str(),
                    Some(repo.path().to_string_lossy().as_ref())
                );
            }
            other => panic!("expected run_command action, got {other:?}"),
        }

        let result = run_next(
            "repo-loop-command-cwd",
            CapturingExecutor::default(),
            &CapturingDispatchHook::default(),
        )
        .expect("run command action");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.status.as_deref(), Some("completed"));
    });
}

#[test]
fn run_command_workflow_times_out_instead_of_blocking_controller() {
    with_isolated_home(|_| {
        let spec = AgentTaskRepoLoopSpec {
            schema: None,
            loop_id: "repo-loop-command-timeout".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
            metadata: Value::Null,
            entities: Vec::new(),
            agents: Vec::new(),
            tools: Vec::new(),
            abilities: Vec::new(),
            workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "deterministic-validation".to_string(),
                agent_id: None,
                prompt: None,
                tasks: vec!["Run deterministic validation.".to_string()],
                entity_ids: Vec::new(),
                fan_out: None,
                tools: Vec::new(),
                abilities: Vec::new(),
                artifacts: vec!["validation_result".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
                runtime_execution: json!({
                    "kind": "command",
                    "command": "/bin/sh",
                    "args": ["-c", "sleep 5"],
                    "timeout_seconds": 1
                }),
                inputs: Value::Null,
            }],
            artifacts: vec![AgentTaskRepoLoopSpecArtifact {
                artifact_id: "validation_result".to_string(),
                kind: "example/ValidationResult/v1".to_string(),
                description: None,
                required: true,
            }],
            artifact_graph: Vec::new(),
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            gate_bundles: Vec::new(),
            policy: None,
            phases: Vec::new(),
            actions: Vec::new(),
            initial_event: None,
        };

        init_from_spec(ControllerFromSpecRequest { spec }).expect("init");
        let result = run_next(
            "repo-loop-command-timeout",
            CapturingExecutor::default(),
            &CapturingDispatchHook::default(),
        )
        .expect("run command action");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status.as_deref(), Some("failed"));
        let execution = result.value.execution.as_ref().expect("execution");
        assert_eq!(execution["timed_out"].as_bool(), Some(true));
        assert_eq!(execution["timeout_seconds"].as_u64(), Some(1));
    });
}

#[test]
fn run_command_workflow_timeout_kills_child_process_group() {
    with_isolated_home(|_| {
        let marker_dir = tempfile::tempdir().expect("marker tempdir");
        let marker = marker_dir.path().join("orphan-marker");
        let marker_arg = marker.to_string_lossy().into_owned();
        let spec = AgentTaskRepoLoopSpec {
            schema: None,
            loop_id: "repo-loop-command-process-group-timeout".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
            metadata: Value::Null,
            entities: Vec::new(),
            agents: Vec::new(),
            tools: Vec::new(),
            abilities: Vec::new(),
            workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                workflow_id: "deterministic-validation".to_string(),
                agent_id: None,
                prompt: None,
                tasks: vec!["Run deterministic validation.".to_string()],
                entity_ids: Vec::new(),
                fan_out: None,
                tools: Vec::new(),
                abilities: Vec::new(),
                artifacts: vec!["validation_result".to_string()],
                consumes: Vec::new(),
                emits: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
                runtime_execution: json!({
                    "kind": "command",
                    "command": "/bin/sh",
                    "args": ["-c", "sleep 3 && touch \"$1\" & wait", "sh", marker_arg],
                    "timeout_seconds": 1
                }),
                inputs: Value::Null,
            }],
            artifacts: vec![AgentTaskRepoLoopSpecArtifact {
                artifact_id: "validation_result".to_string(),
                kind: "example/ValidationResult/v1".to_string(),
                description: None,
                required: true,
            }],
            artifact_graph: Vec::new(),
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            gate_bundles: Vec::new(),
            policy: None,
            phases: Vec::new(),
            actions: Vec::new(),
            initial_event: None,
        };

        init_from_spec(ControllerFromSpecRequest { spec }).expect("init");
        let result = run_next(
            "repo-loop-command-process-group-timeout",
            CapturingExecutor::default(),
            &CapturingDispatchHook::default(),
        )
        .expect("run command action");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status.as_deref(), Some("failed"));
        std::thread::sleep(std::time::Duration::from_secs(4));
        assert!(
            !marker.exists(),
            "timed-out command left an orphan child process"
        );
    });
}
