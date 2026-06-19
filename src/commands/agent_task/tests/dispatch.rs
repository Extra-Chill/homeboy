//! Agent-task command from-spec dispatch defaults and controller dispatch arg tests.

use super::support::*;

#[test]
fn from_spec_dispatch_defaults_use_spec_git_checkout() {
    let repo = tempfile::tempdir().expect("repo dir");
    let git_status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .arg("init")
        .status()
        .expect("git init runs");
    assert!(git_status.success());
    let spec_dir = repo.path().join(".github/homeboy/controllers");
    std::fs::create_dir_all(&spec_dir).expect("spec dir");
    let spec_path = spec_dir.join("loop.json");
    std::fs::write(&spec_path, "{}").expect("spec file");
    let mut spec = AgentTaskRepoLoopSpec {
        schema: None,
        loop_id: "repo-loop-cli-defaults".to_string(),
        phase: "init".to_string(),
        config_version: "v1".to_string(),
        metadata: Value::Null,
        entities: Vec::new(),
        agents: Vec::new(),
        tools: Vec::new(),
        abilities: Vec::new(),
        workflows: Vec::new(),
        artifacts: Vec::new(),
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

    apply_from_spec_dispatch_defaults(&mut spec, &format!("@{}", spec_path.display()));
    let expected_root = std::fs::canonicalize(repo.path()).expect("canonical repo path");

    assert_eq!(
        spec.metadata["dispatch_defaults"]["cwd"],
        expected_root.display().to_string()
    );
    assert_eq!(
        spec.metadata["dispatch_defaults"]["repo"],
        repo.path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string()
    );
}

#[test]
fn from_spec_dispatch_defaults_fall_back_to_current_git_checkout() {
    let repo = tempfile::tempdir().expect("repo dir");
    let git_status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .arg("init")
        .status()
        .expect("git init runs");
    assert!(git_status.success());
    let mut spec = AgentTaskRepoLoopSpec {
        schema: None,
        loop_id: "repo-loop-cli-cwd-defaults".to_string(),
        phase: "init".to_string(),
        config_version: "v1".to_string(),
        metadata: Value::Null,
        entities: Vec::new(),
        agents: Vec::new(),
        tools: Vec::new(),
        abilities: Vec::new(),
        workflows: Vec::new(),
        artifacts: Vec::new(),
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
    spec.workflows.push(
        homeboy::core::agent_tasks::controller_service::AgentTaskRepoLoopSpecWorkflow {
            workflow_id: "store-idea".to_string(),
            agent_id: None,
            prompt: Some("cook the next workflow".to_string()),
            tasks: Vec::new(),
            entity_ids: Vec::new(),
            tools: Vec::new(),
            abilities: Vec::new(),
            artifacts: Vec::new(),
            consumes: Vec::new(),
            emits: Vec::new(),
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            inputs: Value::Null,
        },
    );

    apply_from_spec_dispatch_defaults_with_cwd(&mut spec, "-", || Some(repo.path().to_path_buf()));
    let expected_root = std::fs::canonicalize(repo.path()).expect("canonical repo path");
    let expected_repo = repo
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    assert_eq!(
        spec.metadata["dispatch_defaults"]["cwd"],
        expected_root.display().to_string()
    );
    assert_eq!(spec.metadata["dispatch_defaults"]["repo"], expected_repo);

    with_isolated_home(|_| {
        let report = agent_task_controller_service::init_from_spec(ControllerFromSpecRequest {
            spec: spec.clone(),
        })
        .expect("from-spec initialized");
        match &report.actions[0].action {
            AgentTaskLoopPolicyAction::SpawnTask { request, .. } => {
                assert_eq!(
                    request["dispatch"]["cwd"].as_str(),
                    Some(expected_root.display().to_string().as_str())
                );
                assert_eq!(
                    request["dispatch"]["repo"].as_str(),
                    Some(expected_repo.as_str())
                );
            }
            other => panic!("expected compiled spawn task, got {other:?}"),
        }
    });
}

#[test]
fn controller_dispatch_args_preserve_top_level_workspace_context_in_plan() {
    let repo = tempfile::tempdir().expect("repo dir");
    let repo_path = repo.path().display().to_string();
    let request = json!({
        "mode": "dispatch",
        "cwd": repo_path.clone(),
        "repo": "wp-site-generator@canonical-loop-main-20260616",
        "dispatch": {
            "prompt": "cook the next workflow",
            "backend": "sample-runtime"
        }
    });

    let args = dispatch_args_from_controller_request(&request).expect("dispatch args");
    let dispatch_request = homeboy::core::agent_tasks::dispatch_service::AgentTaskDispatchRequest {
        prompt: args.prompt,
        tasks: args.tasks,
        cwd: args.cwd,
        workspace: args.workspace,
        repo: args.repo,
        task_url: args.task_url,
        backend: args.backend.expect("backend"),
        selector: args.selector,
        model: args.model,
        required_capabilities: args.required_capabilities,
        secret_env: args.secret_env,
        concurrency: args.concurrency,
        run_id: args.run_id,
        core: args.core.into(),
    };
    let plan = homeboy::core::agent_tasks::dispatch_service::build_dispatch_plan_with_provider_requirements(
            &dispatch_request,
            |_backend, _selector| false,
        )
        .expect("dispatch plan");
    let task = plan.tasks.first().expect("plan task");

    assert_eq!(task.workspace.root.as_deref(), Some(repo_path.as_str()));
    assert_eq!(
        task.workspace.slug.as_deref(),
        Some("wp-site-generator@canonical-loop-main-20260616")
    );
    assert_eq!(
        task.executor.config["workspace_root"].as_str(),
        Some(repo_path.as_str())
    );
    assert_eq!(
        task.executor.config["repo"].as_str(),
        Some("wp-site-generator@canonical-loop-main-20260616")
    );
    assert_eq!(
        plan.metadata["workspace_root"].as_str(),
        Some(repo_path.as_str())
    );
}

#[test]
fn controller_events_command_applies_generic_event() {
    with_isolated_home(|_| {
        agent_task_controller_service::init(
            homeboy::core::agent_tasks::controller_service::ControllerInitRequest {
                loop_id: "controller-events-cli".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            },
        )
        .expect("controller initialized");

        let (value, status) = apply_controller_event(AgentTaskControllerApplyEventArgs {
            loop_id: "controller-events-cli".to_string(),
            event_type: "task.completed".to_string(),
            event_id: Some("event-1".to_string()),
            event_key: Some("task#1".to_string()),
            entity_id: Some("entity-1".to_string()),
            payload: Some(r#"{"status":"ok"}"#.to_string()),
        })
        .expect("event applied");

        assert_eq!(status, 0);
        assert_eq!(
            value["schema"],
            homeboy::core::agent_tasks::controller_service::APPLY_EVENT_RESULT_SCHEMA
        );
        assert_eq!(
            value["controller"]["history"][0]["event_type"],
            "task.completed"
        );
        assert_eq!(value["controller"]["history"][0]["entity_id"], "entity-1");
        assert_eq!(value["controller"]["history"][0]["payload"]["status"], "ok");
    });
}
