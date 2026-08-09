use homeboy_agents::agent_task_lifecycle::{
    record_detached_lab_run, record_lab_offload_planned, DetachedLabRunRecord, LabOffloadProxyPlan,
};
use homeboy_agents::agent_task_service::{
    discover_runs, AgentTaskDiscoveryFilter, AgentTaskLiveness,
};
use homeboy_core::test_support::HomeGuard;

#[test]
fn controller_proxy_remains_resolvable_after_detached_runner_acceptance() {
    let _home = HomeGuard::new();
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];

    record_lab_offload_planned(LabOffloadProxyPlan {
        run_id: "accepted-handoff",
        runner_id: "homeboy-lab",
        remote_workspace: "/runner/workspace/homeboy",
        remote_command: &command,
        durable_plan: None,
    })
    .expect("persist controller proxy");

    let active =
        discover_runs(AgentTaskDiscoveryFilter::Active).expect("discover controller proxy");
    let run = active.runs.first().expect("controller proxy");
    assert_eq!(
        run.commands.status,
        "homeboy --placement local agent-task status accepted-handoff"
    );

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
        "homeboy --placement local agent-task status accepted-handoff"
    );
}
