//! Local-run fallback and unsupported-command diagnostics for Lab offload.

use crate::core::plan::HomeboyPlan;
use crate::core::runner::lab_offload_metadata;
use crate::core::Error;

use super::offload::{attach_lab_offload_overhead, LabOffloadOutcome, LabOffloadOverhead};

/// Build the `RunLocal` outcome used whenever automatic Lab offload is skipped
/// for a portability/default-runner reason. Centralizes the repeated
/// "automatic / skipped" metadata shape used by `execute_lab_offload`.
pub(super) fn skipped_automatic_run_local(plan: HomeboyPlan, reason: &str) -> LabOffloadOutcome {
    LabOffloadOutcome::RunLocal {
        metadata: Some(lab_offload_metadata(
            &plan,
            "automatic",
            None,
            None,
            "skipped",
            None,
            Some(reason),
        )),
        plan,
        messages: Vec::new(),
    }
}

/// Same as [`skipped_automatic_run_local`], but also attaches the runner-agnostic
/// Lab offload overhead (#3001) so a skipped/fallback-to-local run still records
/// the per-phase setup timings and fallback reason gathered so far.
pub(super) fn skipped_automatic_run_local_with_overhead(
    plan: HomeboyPlan,
    reason: &str,
    overhead: &LabOffloadOverhead,
) -> LabOffloadOutcome {
    let mut metadata = lab_offload_metadata(
        &plan,
        "automatic",
        None,
        None,
        "skipped",
        None,
        Some(reason),
    );
    attach_lab_offload_overhead(&mut metadata, overhead);
    LabOffloadOutcome::RunLocal {
        metadata: Some(metadata),
        plan,
        messages: Vec::new(),
    }
}

pub(super) fn local_execution_denied_error(reason: &str, runner_id: Option<&str>) -> Error {
    let mut hints = vec![
        "Use --runner <runner-id> to offload to Lab.".to_string(),
        "Remove --lab-only only when local execution on this controller is intentional."
            .to_string(),
    ];
    if let Some(runner_id) = runner_id {
        hints.insert(
            0,
            format!("Reconnect or repair runner `{runner_id}` before retrying."),
        );
    }
    Error::validation_invalid_argument(
        "lab_only",
        format!("Lab-only execution refused local execution: {reason}"),
        runner_id.map(str::to_string),
        Some(hints),
    )
}

pub(super) fn is_build_command(args: &[String]) -> bool {
    args.get(1).is_some_and(|arg| arg == "build")
}

fn build_lab_replacement_hints(runner_id: Option<&str>) -> Vec<String> {
    let runner = runner_id.unwrap_or("<runner-id>");
    vec![
        format!(
            "Materialize the build workspace first: homeboy runner workspace sync {runner} --path <local-worktree> --mode snapshot"
        ),
        format!(
            "Then run the build in the returned runner_path: homeboy runner exec {runner} --cwd <runner_path> -- homeboy build <component>"
        ),
    ]
}

pub(super) fn unsupported_build_lab_error(field: &'static str, runner_id: Option<&str>) -> Error {
    Error::validation_invalid_argument(
        field,
        "homeboy build is not Lab-portable yet; it requires an explicit runner workspace sync and runner exec handoff instead of --runner/--lab-only on build."
            .to_string(),
        runner_id.map(str::to_string),
        Some(build_lab_replacement_hints(runner_id)),
    )
}
