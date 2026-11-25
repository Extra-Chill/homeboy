//! Shared plan-step execution helpers and change provenance for workflow guards.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::plan::{PlanStep, PlanStepDependencyKind, PlanStepStatus};

/// Result of walking executable plan steps through a workflow dispatcher.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanExecutionRun<R> {
    pub results: Vec<R>,
    pub stopped: bool,
}

/// Return dependency IDs that must complete before a step can execute.
///
/// Display-only ordering edges intentionally return no execution dependencies;
/// callers can still use `step.needs` directly when rendering a stable plan.
pub fn executable_plan_step_needs(step: &PlanStep) -> impl Iterator<Item = &str> {
    step.needs
        .iter()
        .filter(move |_| step.needs_kind == PlanStepDependencyKind::Execution)
        .map(String::as_str)
}

/// Execute plan steps with workflow-specific dispatch and stop semantics.
///
/// The shared loop owns generic plan behavior: disabled/skipped steps are not
/// dispatched, caller-filtered steps are skipped, dispatcher `None` means the
/// step was plan-only/non-executable, and `should_stop` decides whether a
/// produced result blocks the rest of the plan.
pub fn execute_plan_steps_filtered<R, Skip, Dispatch, Stop>(
    steps: &[PlanStep],
    should_skip_step: Skip,
    mut dispatch: Dispatch,
    should_stop: Stop,
) -> Result<PlanExecutionRun<R>>
where
    Skip: Fn(&PlanStep) -> bool,
    Dispatch: FnMut(&PlanStep) -> Result<Option<R>>,
    Stop: Fn(&R) -> bool,
{
    let mut results = Vec::new();

    for step in steps {
        if should_skip_step(step)
            || matches!(
                step.status,
                PlanStepStatus::Disabled | PlanStepStatus::Skipped
            )
        {
            continue;
        }

        if let Some(result) = dispatch(step)? {
            let stopped = should_stop(&result);
            results.push(result);
            if stopped {
                return Ok(PlanExecutionRun {
                    results,
                    stopped: true,
                });
            }
        }
    }

    Ok(PlanExecutionRun {
        results,
        stopped: false,
    })
}

/// Execute all non-disabled plan steps through a workflow dispatcher.
pub fn execute_plan_steps<R, Dispatch, Stop>(
    steps: &[PlanStep],
    dispatch: Dispatch,
    should_stop: Stop,
) -> Result<PlanExecutionRun<R>>
where
    Dispatch: FnMut(&PlanStep) -> Result<Option<R>>,
    Stop: Fn(&R) -> bool,
{
    execute_plan_steps_filtered(steps, |_| false, dispatch, should_stop)
}

/// Where a change came from, used by release and changelog workflow guards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeArtifactProvenance {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_plan_executor_skips_disabled_steps() {
        let steps = vec![
            PlanStep::disabled_with_reason("skip", "demo.skip", "off").build(),
            PlanStep::ready("run", "demo.run").build(),
        ];
        let mut dispatched = Vec::new();

        let run = execute_plan_steps(
            &steps,
            |step| {
                dispatched.push(step.id.clone());
                Ok(Some(step.kind.clone()))
            },
            |_| false,
        )
        .expect("execute plan");

        assert_eq!(dispatched, vec!["run"]);
        assert_eq!(run.results, vec!["demo.run"]);
        assert!(!run.stopped);
    }

    #[test]
    fn shared_plan_executor_stops_after_blocking_result() {
        let steps = vec![
            PlanStep::ready("first", "demo.first").build(),
            PlanStep::ready("second", "demo.second").build(),
        ];

        let run = execute_plan_steps(
            &steps,
            |step| Ok(Some(step.kind.clone())),
            |result| result == "demo.first",
        )
        .expect("execute plan");

        assert_eq!(run.results, vec!["demo.first"]);
        assert!(run.stopped);
    }

    #[test]
    fn shared_plan_executor_reports_dispatch_errors() {
        let steps = vec![PlanStep::ready("bad", "demo.bad").build()];

        let err = execute_plan_steps::<String, _, _>(
            &steps,
            |_| Err(crate::Error::internal_unexpected("unsupported demo.bad")),
            |_| false,
        )
        .expect_err("unsupported executable step should fail");

        assert!(err.to_string().contains("unsupported demo.bad"));
    }

    #[test]
    fn executable_needs_exclude_display_order_edges() {
        let execution_step = PlanStep::ready("lint", "review.lint")
            .needs(vec!["review.audit".to_string()])
            .build();
        let display_step = PlanStep::ready("test", "review.test")
            .needs(vec!["review.lint".to_string()])
            .needs_kind(PlanStepDependencyKind::DisplayOrder)
            .build();

        assert_eq!(
            executable_plan_step_needs(&execution_step).collect::<Vec<_>>(),
            vec!["review.audit"]
        );
        assert!(executable_plan_step_needs(&display_step)
            .collect::<Vec<_>>()
            .is_empty());
    }
}
