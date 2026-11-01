use super::*;

#[test]
fn task_secret_env_resolves_on_controller_and_is_forwarded() {
    let name = format!("HOMEBOY_TASK_SECRET_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&name, "task-secret-value");
    let args = vec![
        "homeboy".into(),
        "agent-task".into(),
        "cook".into(),
        "--secret-env".into(),
        name.clone(),
    ];
    let mut handoff =
        build_lab_secret_env_handoff_plan(&[LabSecretEnvSource::AgentTask], &args, HashMap::new())
            .expect("task secret handoff");
    assert_eq!(
        handoff.env_delta.get(&name).map(String::as_str),
        Some("task-secret-value")
    );
    assert!(handoff.secret_env_plan.secret_env_names().contains(&name));
    let plan: AgentTaskPlan = serde_json::from_value(serde_json::json!({
        "schema": "homeboy/agent-task-plan/v1", "plan_id": "forwarded-secret",
        "tasks": [{"schema": AGENT_TASK_REQUEST_SCHEMA, "task_id": "task-a", "executor": {"backend": "pi", "secret_env": [name]}, "instructions": "test"}]
    })).expect("task plan");
    merge_agent_task_secret_env(&mut handoff, &plan);
    assert_eq!(
        handoff.env_delta.get(&name).map(String::as_str),
        Some("task-secret-value")
    );
    assert!(!handoff.runner_deferred_secret_env.contains(&name));
    assert!(!handoff
        .diagnostics
        .to_string()
        .contains("task-secret-value"));
    std::env::remove_var(&name);
}

#[test]
fn configured_task_secret_is_forwarded_from_controller() {
    crate::test_support::with_isolated_home(|_| {
        let name = format!("HOMEBOY_CONFIGURED_TASK_{}", uuid::Uuid::new_v4().simple());
        agent_task_secrets::set_config_secret(&name, "configured-task-value")
            .expect("configured source");
        let args = vec![
            "homeboy".into(),
            "agent-task".into(),
            "cook".into(),
            format!("--secret-env={name}"),
        ];
        let handoff = build_lab_secret_env_handoff_plan(
            &[LabSecretEnvSource::AgentTask],
            &args,
            HashMap::new(),
        )
        .expect("configured task handoff");
        assert_eq!(
            handoff.env_delta.get(&name).map(String::as_str),
            Some("configured-task-value")
        );
        assert!(!handoff
            .diagnostics
            .to_string()
            .contains("configured-task-value"));
    });
}

#[test]
fn run_plan_task_secrets_keep_task_provenance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("plan.json");
    std::fs::write(&path, serde_json::json!({
        "schema": "homeboy/agent-task-plan/v1", "plan_id": "task-secrets",
        "tasks": [{"schema": AGENT_TASK_REQUEST_SCHEMA, "task_id": "task-a", "executor": {"backend": "pi", "secret_env": ["TASK_TOKEN"]}, "instructions": "test"}]
    }).to_string()).expect("write plan");
    let args = run_plan_args(&path);
    assert_eq!(
        declared_agent_task_run_plan_secret_env(&args),
        vec!["TASK_TOKEN"]
    );
    let error = preflight_agent_task_runner_secret_env_plan(
        "lab-a",
        &fixture_runner(HashMap::new()),
        &args,
        &HashMap::new(),
        &secret_env_plan_from_args(&args),
    )
    .expect_err("missing runner ref");
    assert_eq!(
        error.details["required_by_tasks"]["TASK_TOKEN"],
        serde_json::json!(["task-a"])
    );
    preflight_agent_task_runner_secret_env_plan(
        "lab-a",
        &fixture_runner(HashMap::new()),
        &args,
        &HashMap::from([("TASK_TOKEN".into(), "forwarded".into())]),
        &secret_env_plan_from_args(&args),
    )
    .expect("forwarded task secret");
}
