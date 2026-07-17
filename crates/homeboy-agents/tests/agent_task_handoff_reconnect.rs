use homeboy_agents::agent_task_lifecycle::{
    record_detached_lab_run, record_lab_offload_planned, status, DetachedLabRunRecord,
    LabOffloadProxyPlan,
};
use homeboy_agents::agent_task_service::{
    discover_runs, reconcile_stale_active_runs, AgentTaskDiscoveryFilter, AgentTaskLiveness,
};

struct EnvironmentGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvironmentGuard {
    fn isolated() -> Self {
        let root = tempfile::tempdir().expect("temporary home");
        let root = root.keep();
        let values = [
            ("HOME", Some(root.clone().into_os_string())),
            (
                "XDG_CONFIG_HOME",
                Some(root.join("config").into_os_string()),
            ),
            ("XDG_DATA_HOME", Some(root.join("data").into_os_string())),
            (
                "HOMEBOY_ARTIFACT_ROOT",
                Some(root.join("artifacts").into_os_string()),
            ),
            (
                "HOMEBOY_RUNTIME_TMPDIR",
                Some(root.join("runtime").into_os_string()),
            ),
            (
                "HOMEBOY_TEST_LAB_HANDOFF_ACCEPTANCE_TIMEOUT_SECONDS",
                Some("0".into()),
            ),
        ];
        let previous = values
            .iter()
            .map(|(name, value)| {
                let prior = std::env::var_os(name);
                std::env::set_var(name, value.as_ref().expect("test value"));
                (*name, prior)
            })
            .collect();
        Self(previous)
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

#[test]
fn controller_handoff_remains_resolvable_and_reconciles_when_unaccepted() {
    let _environment = EnvironmentGuard::isolated();
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];

    record_lab_offload_planned(LabOffloadProxyPlan {
        run_id: "unaccepted-handoff",
        runner_id: "homeboy-lab",
        remote_workspace: "/runner/workspace/homeboy",
        remote_command: &command,
        durable_plan: None,
    })
    .expect("persist controller proxy");

    let active = discover_runs(AgentTaskDiscoveryFilter::Active).expect("discover unaccepted run");
    let run = active.runs.first().expect("unaccepted run");
    assert_eq!(run.liveness, Some(AgentTaskLiveness::Unreconciled));
    assert_eq!(
        run.commands.status,
        "homeboy agent-task status unaccepted-handoff"
    );

    let reconciled = reconcile_stale_active_runs(false).expect("reconcile unaccepted handoff");
    assert_eq!(reconciled.reconciled, 1);
    assert_eq!(
        status("unaccepted-handoff")
            .expect("terminal controller record")
            .state,
        homeboy_agents::agent_task_lifecycle::AgentTaskRunState::Cancelled
    );

    record_lab_offload_planned(LabOffloadProxyPlan {
        run_id: "accepted-handoff",
        runner_id: "homeboy-lab",
        remote_workspace: "/runner/workspace/homeboy",
        remote_command: &command,
        durable_plan: None,
    })
    .expect("persist second controller proxy");
    record_detached_lab_run(DetachedLabRunRecord {
        run_id: "accepted-handoff",
        runner_id: "homeboy-lab",
        runner_job_id: "accepted-daemon-job",
        remote_workspace: "/runner/workspace/homeboy",
        remote_command: &command,
    })
    .expect("record authoritative runner acceptance");

    let active = discover_runs(AgentTaskDiscoveryFilter::Active).expect("discover accepted run");
    let run = active
        .runs
        .iter()
        .find(|run| run.run_id == "accepted-handoff")
        .expect("accepted run");
    assert_eq!(run.liveness, Some(AgentTaskLiveness::Active));
    assert_eq!(run.runner_job_id.as_deref(), Some("accepted-daemon-job"));
    assert_eq!(
        run.commands.status,
        "homeboy agent-task status accepted-handoff"
    );
}
