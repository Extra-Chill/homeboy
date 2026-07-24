//! Cook candidate-workspace restoration for retry/continuation. Extracted from
//! `lifecycle_ops` to keep that module within the god-file threshold (#9927).

use std::path::Path;

use serde_json::Value;

use homeboy_core::{Error, Result};

use crate::agent_task_scheduler::AgentTaskPlan;

pub(super) fn restore_follow_up_cook_candidate_workspace(plan: &mut AgentTaskPlan) -> Result<()> {
    let candidate_tasks = plan
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            task.inputs
                .pointer("/cook_loop/artifact_provenance/source_run_id")
                .and_then(Value::as_str)
                .is_some()
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if candidate_tasks.len() > 1 {
        return Err(Error::validation_invalid_argument(
            "cook_loop.artifact_provenance",
            "Cook retry plan has ambiguous prior candidates",
            None,
            None,
        ));
    }
    let Some(&index) = candidate_tasks.first() else {
        return Ok(());
    };
    let task = &mut plan.tasks[index];
    let provenance = task
        .inputs
        .pointer("/cook_loop/artifact_provenance")
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_loop.artifact_provenance",
                "Cook retry candidate has no prior artifact evidence",
                None,
                None,
            )
        })?;
    let source_run_id = provenance
        .get("source_run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_loop.artifact_provenance.source_run_id",
                "Cook retry candidate has no source run evidence",
                None,
                None,
            )
        })?;
    let source_task_id = provenance
        .get("source_task_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_loop.artifact_provenance.source_task_id",
                "Cook retry candidate has no source task evidence",
                None,
                None,
            )
        })?;
    let source_sha256 = provenance
        .get("source_patch_artifact_sha256")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_loop.artifact_provenance.source_patch_artifact_sha256",
                "Cook retry candidate has no patch identity evidence",
                None,
                None,
            )
        })?;
    let promotion = crate::agent_task_service::persisted_promotion_for_attempt(source_run_id)?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_loop.artifact_provenance",
                "Cook retry candidate has no durable prior promotion",
                Some(source_run_id.to_string()),
                None,
            )
        })?;
    if promotion.source.task_id != source_task_id
        || promotion.patch_artifact.sha256.as_deref() != Some(source_sha256)
    {
        return Err(Error::validation_invalid_argument(
            "cook_loop.artifact_provenance",
            "Cook retry candidate evidence does not match its prior promotion",
            Some(source_run_id.to_string()),
            None,
        ));
    }
    let baseline = crate::agent_task_service::materialize_follow_up_baseline(
        &promotion,
        source_run_id,
        &task.task_id,
    )?;
    task.workspace.root = Some(baseline.path.display().to_string());
    task.executor.remap_workspace_root(
        task.workspace
            .root
            .as_deref()
            .expect("baseline path is assigned"),
    );
    task.metadata["verified_cook_baseline"] = baseline.capability().verified_baseline_provenance();
    baseline.preserve_for_retry();
    Ok(())
}

/// Cook's first dirty-candidate baseline is process-local and removed after a
/// failed admission. A retry returns to the durable candidate source workspace;
/// the original task workspace remains in metadata for routing and projection.
pub(super) fn restore_initial_cook_candidate_workspace(plan: &mut AgentTaskPlan) -> Result<()> {
    for task in &mut plan.tasks {
        let Some(baseline) = task.metadata.get("cook_initial_candidate_baseline") else {
            continue;
        };
        let continuation_root = task
            .metadata
            .pointer("/cook_continuation_workspace/candidate_source_root")
            .and_then(Value::as_str)
            // The first continuation snapshot used `root` for the task
            // workspace. New records use candidate_source_root; retain the
            // legacy form only when no source-root evidence is available.
            .or_else(|| {
                baseline
                    .get("source_root")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        task.metadata
                            .pointer("/cook_continuation_workspace/root")
                            .and_then(Value::as_str)
                    })
            })
            .or_else(|| {
                task.workspace
                    .materialization
                    .get("root")
                    .and_then(Value::as_str)
            })
            // Older records did not retain a continuation workspace separately.
            .or_else(|| baseline.get("source_root").and_then(Value::as_str))
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| missing_cook_candidate_source_workspace(&task.task_id, None))?;
        if !std::path::Path::new(continuation_root).is_dir() {
            return Err(missing_cook_candidate_source_workspace(
                &task.task_id,
                Some(continuation_root),
            ));
        }
        task.workspace.root = Some(continuation_root.to_string());
        task.executor.remap_workspace_root(continuation_root);
    }
    Ok(())
}

fn missing_cook_candidate_source_workspace(task_id: &str, root: Option<&str>) -> Error {
    let root_description = root.map(|root| format!(" at {root}")).unwrap_or_default();
    let mut error = Error::validation_invalid_argument(
        "workspace",
        format!(
            "Cook retry candidate source workspace for task '{task_id}' is unavailable{root_description}"
        ),
        root.map(str::to_string),
        None,
    );
    // Losing a managed workspace is lifecycle recovery work, not malformed user
    // input. Callers persist this as a retryable pre-execution failure.
    error.retryable = Some(true);
    error
        .with_hint("Restore the recorded candidate source workspace, then retry the run.")
        .with_hint(
            "If the original --cwd is unavailable, rerun Cook from a replacement workspace with its explicit --cwd.",
        )
}
