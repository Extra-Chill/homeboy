use crate::agent_task_lifecycle::record_runner_job_identity;
use crate::agent_tasks::lifecycle as agent_task_lifecycle;
use crate::agent_tasks::scheduler::AgentTaskAggregate;
use crate::lab_contract::AgentTaskDispatchIdentity;
use crate::notification_route::NotificationRoute;
use crate::{config, Error, Result};

pub fn mirror_agent_task_run_plan_aggregate(
    plan_spec: &str,
    run_id: &str,
    aggregate: AgentTaskAggregate,
    notification_route: Option<&NotificationRoute>,
    dispatch_identity: Option<&AgentTaskDispatchIdentity>,
) -> Result<()> {
    // A controller-created run owns its durable plan. The runner's staged path
    // only transports that plan and may disappear before its result is mirrored.
    // Re-submitting it here would replace the controller retry input.
    let controller_owned = agent_task_lifecycle::run_record_exists(run_id)?;
    let plan = if controller_owned {
        agent_task_lifecycle::load_plan(run_id)?
    } else {
        let raw_plan = config::read_json_spec_to_string(plan_spec)?;
        serde_json::from_str(&raw_plan).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("read agent-task plan {plan_spec}")),
            )
        })?
    };
    if !controller_owned {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
    }
    if let Some(notification_route) = notification_route {
        crate::agent_task_lifecycle::persist_notification_route(run_id, notification_route)?;
    }
    if !controller_owned {
        agent_task_lifecycle::mark_running(run_id)?;
    }
    if let Some(identity) = dispatch_identity.filter(|identity| {
        !identity.runner_id.trim().is_empty() && !identity.runner_job_id.trim().is_empty()
    }) {
        record_runner_job_identity(run_id, &identity.runner_id, &identity.runner_job_id)?;
    }
    // Artifact projection needs the runner identity while reconciling the
    // aggregate so it can distinguish runner provenance from controller bytes.
    agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_tasks::scheduler::{
        AgentTaskAggregateStatus, AgentTaskAggregateTotals, AgentTaskPlan,
    };

    #[test]
    fn detached_projection_retries_from_controller_plan_after_runner_staging_is_removed() {
        crate::test_support::with_isolated_home(|_| {
            let controller_plan = AgentTaskPlan::new("controller-plan", Vec::new());
            let submitted =
                agent_task_lifecycle::submit_plan(&controller_plan, Some("detached-run"))
                    .expect("controller plan submitted");
            let staged = tempfile::NamedTempFile::new().expect("runner staged plan");
            let staged_spec = format!("@{}", staged.path().display());
            drop(staged);
            let aggregate = AgentTaskAggregate {
                schema: "homeboy/agent-task-aggregate/v1".to_string(),
                plan_id: "runner-staged-plan".to_string(),
                status: AgentTaskAggregateStatus::Failed,
                totals: AgentTaskAggregateTotals {
                    failed: 1,
                    ..Default::default()
                },
                outcomes: Vec::new(),
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            };

            mirror_agent_task_run_plan_aggregate(
                &staged_spec,
                "detached-run",
                aggregate,
                None,
                None,
            )
            .expect("detached projection uses the controller plan");

            let projected = agent_task_lifecycle::status("detached-run").expect("projection");
            assert_eq!(projected.plan_id, controller_plan.plan_id);
            assert_eq!(projected.plan_path, submitted.plan_path);
            let retry = agent_task_lifecycle::retry("detached-run", Some("local-retry"))
                .expect("local retry rematerializes controller plan");
            assert_eq!(
                agent_task_lifecycle::load_plan(&retry.run_id).expect("retry plan"),
                controller_plan
            );
        });
    }

    #[test]
    fn detached_projection_fails_closed_when_controller_plan_is_missing() {
        crate::test_support::with_isolated_home(|_| {
            let controller_plan = AgentTaskPlan::new("controller-plan", Vec::new());
            let submitted =
                agent_task_lifecycle::submit_plan(&controller_plan, Some("detached-run"))
                    .expect("controller plan submitted");
            std::fs::remove_file(&submitted.plan_path).expect("remove controller plan");
            let staged = tempfile::NamedTempFile::new().expect("runner staged plan");
            std::fs::write(
                staged.path(),
                serde_json::to_string(&controller_plan).unwrap(),
            )
            .expect("write runner staged plan");
            let aggregate = AgentTaskAggregate {
                schema: "homeboy/agent-task-aggregate/v1".to_string(),
                plan_id: "runner-staged-plan".to_string(),
                status: AgentTaskAggregateStatus::Failed,
                totals: Default::default(),
                outcomes: Vec::new(),
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            };

            let error = mirror_agent_task_run_plan_aggregate(
                &format!("@{}", staged.path().display()),
                "detached-run",
                aggregate,
                None,
                None,
            )
            .expect_err("missing controller plan must not fall back to runner staging");

            assert_eq!(error.code.as_str(), "internal.io_error");
            assert_eq!(
                agent_task_lifecycle::retry("detached-run", Some("local-retry"))
                    .expect_err("retry also fails closed")
                    .code
                    .as_str(),
                "internal.io_error"
            );
        });
    }
}
