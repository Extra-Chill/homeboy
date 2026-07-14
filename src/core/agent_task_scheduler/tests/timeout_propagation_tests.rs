use super::fixtures::{plan_with_tasks, RotationScriptedExecutor};
use crate::core::agent_task::{AgentTaskFailureClassification, AgentTaskOutcomeStatus};
use crate::core::agent_task_scheduler::{
    AgentTaskProviderRotationEntry, AgentTaskProviderRotationPolicy, AgentTaskScheduler,
};
use std::sync::Arc;

#[test]
fn plan_timeout_reaches_each_rotated_provider_attempt() {
    let executor = RotationScriptedExecutor::new(vec![(
        AgentTaskOutcomeStatus::ProviderError,
        Some(AgentTaskFailureClassification::Provider),
    )]);
    let observed = Arc::clone(&executor.observed);
    let scheduler = AgentTaskScheduler::new(executor);
    let mut plan = plan_with_tasks(1);
    plan.options.timeout_ms = Some(2_700_000);
    plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
        entries: vec![
            AgentTaskProviderRotationEntry {
                backend: Some("fallback-a".to_string()),
                ..Default::default()
            },
            AgentTaskProviderRotationEntry {
                backend: Some("fallback-b".to_string()),
                ..Default::default()
            },
        ],
        ..Default::default()
    });

    scheduler.run(plan);

    assert!(observed
        .lock()
        .expect("observed requests")
        .iter()
        .all(|request| request.limits.timeout_ms == Some(2_700_000)));
}
