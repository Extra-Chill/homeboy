//! `homeboy runs profile`: where CPU goes across recent runs (#18461).

use clap::Args;
use serde::Serialize;

use homeboy::core::observation::ObservationStore;
use homeboy::core::performance_hotspots::{
    resource_profile, ResourceProfile, ResourceProfileQuery,
};

use super::{CmdResult, RunsOutput};

#[derive(Args, Clone, Debug)]
pub struct RunsProfileArgs {
    /// Only runs started within this window (e.g. 24h, 7d).
    #[arg(long, default_value = "24h")]
    pub since: String,
    /// Only instances of this workflow definition.
    #[arg(long)]
    pub workflow: Option<String>,
    /// Only runs of this component.
    #[arg(long)]
    pub component: Option<String>,
    /// Most step or phase names to report (bounded at 200).
    #[arg(long, default_value_t = 30)]
    pub limit: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunsProfileOutput {
    pub command: &'static str,
    pub since: String,
    #[serde(flatten)]
    pub profile: ResourceProfile,
}

pub(crate) fn runs_profile(
    store: &ObservationStore,
    args: RunsProfileArgs,
) -> CmdResult<RunsOutput> {
    let since = super::common::since_threshold(&args.since)?;
    let profile = resource_profile(
        store,
        &ResourceProfileQuery {
            since: Some(since.clone()),
            workflow: args.workflow,
            component: args.component,
            limit: args.limit,
        },
    )?;
    Ok((
        RunsOutput::Profile(RunsProfileOutput {
            command: "runs.profile",
            since,
            profile,
        }),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use homeboy::core::daemon::step_job::StepJobRequest;
    use homeboy::core::daemon::step_job::{recorded_step_usage, step_command, step_job_dir};
    use homeboy::core::workflow::{
        advance, Command, Event, FinishedStatus, InstanceState, StepResult, SubmissionKind,
        WorkflowDefinition,
    };
    use homeboy::test_support::with_isolated_home;
    use serde_json::json;

    use super::*;

    /// A shell loop of `iterations` steps: CPU cost proportional to its size.
    fn burn(iterations: u32) -> serde_json::Value {
        json!({"program": "/bin/sh", "args": ["-c",
            format!("i=0; while [ $i -lt {iterations} ]; do i=$((i+1)); done")]})
    }

    #[test]
    fn workflow_step_cpu_is_attributed_by_runs_show_and_runs_profile() {
        with_isolated_home(|home| {
            let definition = WorkflowDefinition::from_value(json!({
                "schema": "homeboy/workflow/v1", "id": "cpu-proof",
                "steps": [
                    {"id": "light", "exec": burn(20_000)},
                    {"id": "heavy", "needs": ["light"], "exec": burn(400_000)},
                ]
            }))
            .unwrap();
            definition.validate(&|_| true).unwrap();
            let instance = definition.instance_id(&BTreeMap::new(), None);
            let mut state = InstanceState::new(&definition, &instance, BTreeMap::new());
            let mut event = Event::Start;
            let started = std::time::Instant::now();
            let now = || started.elapsed().as_millis() as u64;
            // Drive the real engine; each step child runs in the real step
            // wrapper, which records its CPU at finish.
            for _ in 0..8 {
                let (next, commands) = advance(&definition, &state, &event, now());
                state = next;
                let Some(submission) = commands.iter().find_map(|command| match command {
                    Command::SubmitStep(submission) => Some(submission.clone()),
                    _ => None,
                }) else {
                    break;
                };
                let SubmissionKind::Exec { program, args, .. } = &submission.kind else {
                    panic!("exec steps only");
                };
                let job_id = format!("cpu-proof-{}", submission.step);
                let dir = step_job_dir(&job_id).unwrap();
                std::fs::create_dir_all(&dir).unwrap();
                let request = StepJobRequest::new(
                    &submission.key,
                    program,
                    args.clone(),
                    home.path().display().to_string(),
                    Default::default(),
                    "workflow",
                );
                let spawned = std::time::Instant::now();
                assert!(step_command(&request, &dir).status().unwrap().success());
                let usage =
                    recorded_step_usage(&job_id, spawned.elapsed().as_millis() as u64).unwrap();
                event = Event::StepFinished(StepResult {
                    key: submission.key.clone(),
                    error: None,
                    status: FinishedStatus::Succeeded,
                    outcome: None,
                    output: None,
                    run_id: None,
                    usage: Some(usage),
                });
            }
            assert!(state.status.is_terminal(), "{:?}", state.status);
            let store = ObservationStore::open_initialized().unwrap();
            store.record_workflow_timing("fixture", &state).unwrap();
            // A Cook the instance launched records which part of its CPU was
            // Homeboy's own and which was staging.
            let cook = store
                .start_run_with_id(
                    homeboy::core::observation::NewRunRecord::builder("agent-task").build(),
                    format!("{instance}-heavy-i1-a1-attempt-1-0123abcd"),
                )
                .unwrap();
            let span = |id: &str, cpu: u64, homeboy: u64| {
                json!({"id": id, "attempt_id": "attempt-1", "status": "ok", "started_at": "2026-11-19T00:00:00.000Z",
                    "usage": {"cpu_usage_usec": cpu, "cpu_user_usec": cpu, "cpu_system_usec": 0, "homeboy_cpu_usec": homeboy, "wall_ms": 1,
                        "source": "rusage", "estimated": true}})
            };
            let report = serde_json::from_value(json!({"spans": [
                span("publish.aggregate", 20_000, 20_000),
                span("stage.workspace", 10_000, 0),
                span("provider.command", 0, 0),
                span("gate.test", 0, 0),
                span("unknown", 0, 0),
                ({ let mut value = span("gate.retry", 0, 0);
                  value["attempt_id"] = json!("attempt-2"); value }),
                ({ let mut value = span("schedule", 0, 0);
                  value.as_object_mut().unwrap().remove("attempt_id"); value }),
            ]}))
            .unwrap();
            store
                .record_phase_spans(&cook.id, "attempt", &report)
                .unwrap();

            let (RunsOutput::Show(show), _) =
                super::super::handlers::show_run_in_store(&store, &instance).unwrap()
            else {
                panic!("runs show output");
            };
            let resources = show.run.resources.expect("step usage on runs show");
            let phases = resources
                .phases
                .iter()
                .map(|row| (row.phase.as_str(), row.cpu_seconds))
                .collect::<Vec<_>>();
            assert_eq!(
                phases.iter().map(|(phase, _)| *phase).collect::<Vec<_>>(),
                ["workflow.step.heavy", "workflow.step.light"],
                "sorted by CPU: {phases:?}"
            );
            let (heavy, light) = (phases[0].1, phases[1].1);
            assert!(heavy > 4.0 * light && heavy > 0.05, "{phases:?}");
            assert!(resources
                .phases
                .iter()
                .all(|row| row.estimated && row.instances == 1));
            assert!((resources.cpu_seconds - heavy - light).abs() < 1e-9);
            let split = |overhead: &homeboy::core::performance_hotspots::CpuOverhead| {
                assert!(
                    (overhead.homeboy_cpu_seconds - 0.02).abs() < 1e-9,
                    "{overhead:?}"
                );
                assert!(
                    (overhead.staging_cpu_seconds - 0.01).abs() < 1e-9,
                    "{overhead:?}"
                );
                assert!(
                    (overhead.workload_cpu_seconds - (heavy + light - 0.03)).abs() < 1e-9,
                    "{overhead:?}"
                );
            };
            split(&resources.overhead);
            let cooks = &resources.cooks;
            assert_eq!(cooks.len(), 1);
            assert_eq!(cooks[0].run_id, cook.id);
            assert_eq!(cooks[0].attempts.len(), 3);
            assert_eq!(cooks[0].attempts[0].attempt_id, None);
            assert_eq!(
                cooks[0].attempts[2].attempt_id.as_deref(),
                Some("attempt-2")
            );
            let attempt = &cooks[0].attempts[1];
            assert_eq!(attempt.attempt_id.as_deref(), Some("attempt-1"));
            assert_eq!(attempt.buckets[0].bucket, "setup");
            assert_eq!(attempt.buckets[0].child_cpu_seconds, Some(0.01));
            assert_eq!(attempt.buckets[3].bucket, "finalize");
            assert_eq!(attempt.buckets[3].homeboy_cpu_seconds, 0.02);
            assert_eq!(
                attempt.buckets.iter().map(|b| b.bucket).collect::<Vec<_>>(),
                ["setup", "agent", "verify", "finalize", "other"]
            );
            assert_eq!(attempt.buckets.iter().map(|b| b.wall_ms).sum::<f64>(), 5.0);
            let (RunsOutput::Show(cook_show), _) =
                super::super::handlers::show_run_in_store(&store, &cook.id).unwrap()
            else {
                panic!("Cook runs show output");
            };
            assert_eq!(cook_show.run.resources.unwrap().cooks, *cooks);

            let (RunsOutput::Profile(profile), _) = runs_profile(
                &store,
                RunsProfileArgs {
                    since: "24h".to_string(),
                    workflow: Some("cpu-proof".to_string()),
                    component: None,
                    limit: 30,
                },
            )
            .unwrap() else {
                panic!("runs profile output");
            };
            assert_eq!(profile.profile.runs_with_usage, 1);
            let rows = &profile.profile.phases;
            assert_eq!(
                rows.iter()
                    .map(|row| row.phase.as_str())
                    .collect::<Vec<_>>(),
                ["workflow.step.heavy", "workflow.step.light"]
            );
            assert_eq!(rows[0].instances, 1);
            assert_eq!(rows[0].cpu_seconds_total, heavy);
            assert_eq!(
                (rows[0].cpu_seconds.p50, rows[0].cpu_seconds.p95),
                (heavy, heavy)
            );
            assert!(rows[0].wall_ms.p50 > 0.0);
            split(&profile.profile.overhead);
            assert_eq!(profile.profile.cooks, *cooks);
        });
    }
}
