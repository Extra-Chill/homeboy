//! Durable, provider-neutral follow-up actions for resolved fanout dependencies.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent_task_batch;
use homeboy_core::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyResolution {
    pub child_id: String,
    /// Exact upstream head or merge revision the dependent is rebased onto.
    pub upstream_revision: String,
    /// The PR base after this transition: an upstream candidate branch before
    /// merge, and the resolved target branch after merge.
    pub target_base: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyAction {
    pub upstream_id: String,
    pub downstream_id: String,
    pub worktree: String,
    pub head: String,
    pub upstream_revision: String,
    pub target_base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<String>,
}

/// External mutations owned by the caller. Every call is journaled separately
/// by [`execute_resolved_dependency_actions`], rather than making rebase/push
/// one unrecoverable operation.
pub trait DependencyActionExecutor {
    /// Check a claimed external mutation against its desired after-state before
    /// retrying it after coordinator loss.
    fn side_effect_applied(&mut self, action: &DependencyAction, step: &str) -> Result<bool>;
    fn fetch(&mut self, action: &DependencyAction) -> Result<()>;
    fn rebase(&mut self, action: &DependencyAction) -> Result<()>;
    fn push(&mut self, action: &DependencyAction) -> Result<()>;
    fn update_pull_request_base(&mut self, action: &DependencyAction) -> Result<()>;
    fn invalidate_review(&mut self, action: &DependencyAction) -> Result<()>;
}

const STEPS: [&str; 6] = [
    "fetch",
    "rebase",
    "push",
    "pull_request_base_update",
    "gates_invalidate",
    "review_invalidate",
];

/// Execute transitions from their first incomplete durable step. A claimed
/// record is written before every side effect and a completed record after it;
/// completed pushes are therefore never issued again on a later resume.
pub fn execute_resolved_dependency_actions<E: DependencyActionExecutor>(
    batch_id: &str,
    resolutions: &[DependencyResolution],
    executor: &mut E,
) -> Result<Vec<Value>> {
    let batch = agent_task_batch::read_batch_record(batch_id)?;
    let nodes = batch.metadata["dependency_graph"]["nodes"]
        .as_array()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "batch_id",
                "fanout batch has no dependency graph",
                Some(batch_id.into()),
                None,
            )
        })?;
    let mut actions = Vec::new();
    for resolution in resolutions {
        if resolution.upstream_revision.trim().is_empty()
            || resolution.target_base.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "dependency",
                "dependency transition must include an exact upstream revision and target base",
                Some(resolution.child_id.clone()),
                None,
            ));
        }
        let tracker = nodes
            .iter()
            .find(|node| node["id"].as_str() == Some(&resolution.child_id))
            .and_then(|node| node["tracker_url"].as_str());
        for node in nodes {
            if !node["depends_on"].as_array().is_some_and(|dependencies| {
                dependencies.iter().any(|dependency| {
                    dependency.as_str() == Some(&resolution.child_id)
                        || dependency.as_str() == tracker
                })
            }) {
                continue;
            }
            let downstream_id = node["id"].as_str().unwrap_or_default();
            let worktree = required_node_value(node, "worktree", downstream_id)?;
            let head = required_node_value(node, "head", downstream_id)?;
            let pull_request = batch
                .child_runs
                .iter()
                .find(|child| child.task_id == downstream_id)
                .and_then(|child| crate::agent_task_lifecycle::status(&child.run_id).ok())
                .and_then(|record| record.metadata.get("cook_finalization").cloned())
                .and_then(|value| {
                    value["pr_url"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| value["pr_number"].as_u64().map(|number| number.to_string()))
                        .or_else(|| value["pr"]["url"].as_str().map(str::to_string))
                        .or_else(|| {
                            value["pr"]["number"]
                                .as_u64()
                                .map(|number| number.to_string())
                        })
                })
                // Prior stack transitions retain the PR identity after their
                // gate invalidation removes the old finalization record. A
                // later upstream merge must still move that same PR to target.
                .or_else(|| {
                    batch.metadata["dependency_action_receipts"]
                        .as_object()
                        .into_iter()
                        .flat_map(|receipts| receipts.values())
                        .filter(|receipt| {
                            receipt["action"]["downstream_id"].as_str() == Some(downstream_id)
                        })
                        .rev()
                        .find_map(|receipt| {
                            receipt["action"]["pull_request"]
                                .as_str()
                                .map(str::to_string)
                        })
                });
            actions.push(DependencyAction {
                upstream_id: resolution.child_id.clone(),
                downstream_id: downstream_id.to_string(),
                worktree,
                head,
                upstream_revision: resolution.upstream_revision.clone(),
                target_base: resolution.target_base.clone(),
                pull_request,
            });
        }
    }

    let mut receipts = Vec::new();
    for action in actions {
        let key = format!(
            "{}:{}:{}:{}",
            action.upstream_id, action.downstream_id, action.upstream_revision, action.target_base
        );
        let mut receipt = agent_task_batch::dependency_action_receipt(batch_id, &key)?
            .unwrap_or_else(|| json!({ "status": "pending", "action": action, "steps": {} }));
        for step in STEPS {
            if receipt["steps"][step]["status"] == "completed"
                || (step == "pull_request_base_update" && action.pull_request.is_none())
            {
                continue;
            }
            if matches!(
                receipt["steps"][step]["status"].as_str(),
                Some("claimed" | "blocked")
            ) && executor.side_effect_applied(&action, step)?
            {
                receipt["steps"][step] = json!({ "status": "completed", "before": { "action": action }, "after": { "action": action }, "reconciled": true });
                agent_task_batch::record_dependency_action_receipt(
                    batch_id,
                    &key,
                    receipt.clone(),
                )?;
                continue;
            }
            receipt["status"] = Value::String("running".into());
            receipt["steps"][step] = json!({ "status": "claimed", "before": { "action": action } });
            agent_task_batch::record_dependency_action_receipt(batch_id, &key, receipt.clone())?;
            let result = match step {
                "fetch" => executor.fetch(&action),
                "rebase" => executor.rebase(&action),
                "push" => executor.push(&action),
                "pull_request_base_update" => executor.update_pull_request_base(&action),
                "gates_invalidate" => invalidate_downstream_cook(batch_id, &action),
                "review_invalidate" => executor.invalidate_review(&action),
                _ => unreachable!(),
            };
            match result {
                Ok(()) => {
                    receipt["steps"][step] = json!({ "status": "completed", "before": { "action": action }, "after": { "action": action } });
                    agent_task_batch::record_dependency_action_receipt(
                        batch_id,
                        &key,
                        receipt.clone(),
                    )?;
                }
                Err(error) => {
                    receipt["status"] = Value::String("blocked".into());
                    receipt["blocked_step"] = Value::String(step.into());
                    receipt["steps"][step] = json!({ "status": "blocked", "before": { "action": action }, "error": error.message });
                    receipt["resolution"] = Value::String(format!(
                        "resolve {} for '{}' and resume fanout {}",
                        step, action.worktree, batch_id
                    ));
                    agent_task_batch::record_dependency_action_receipt(
                        batch_id,
                        &key,
                        receipt.clone(),
                    )?;
                    break;
                }
            }
        }
        if STEPS.iter().all(|step| {
            receipt["steps"][step]["status"] == "completed"
                || (*step == "pull_request_base_update" && action.pull_request.is_none())
        }) {
            receipt["status"] = Value::String("completed".into());
            receipt["gates_invalidated"] = Value::Bool(true);
            receipt["review_invalidated"] = Value::Bool(true);
            agent_task_batch::record_dependency_action_receipt(batch_id, &key, receipt.clone())?;
        }
        receipts.push(receipt);
    }
    Ok(receipts)
}

fn required_node_value(node: &Value, field: &str, downstream_id: &str) -> Result<String> {
    node[field]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                format!("dependency_graph.nodes.{field}"),
                "dependent action requires a materialized worktree and head branch",
                Some(downstream_id.into()),
                None,
            )
        })
}

/// This is the lifecycle mutation, not just a display marker: it removes the
/// completed finalization and re-arms Cook's promoted-candidate verification.
fn invalidate_downstream_cook(batch_id: &str, action: &DependencyAction) -> Result<()> {
    let batch = agent_task_batch::read_batch_record(batch_id)?;
    let Some(child) = batch
        .child_runs
        .iter()
        .find(|child| child.task_id == action.downstream_id)
    else {
        return Ok(());
    };
    crate::agent_task_lifecycle::invalidate_cook_finalization_for_dependency(
        &child.run_id,
        &action.upstream_revision,
        &format!("homeboy agent-task fanout resume {batch_id}"),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_batch::{persist_fanout_run_batch, FanoutRunBatchChild};

    #[derive(Default)]
    struct Fake {
        calls: Vec<&'static str>,
        fail: Option<&'static str>,
        applied: Vec<&'static str>,
    }
    impl DependencyActionExecutor for Fake {
        fn side_effect_applied(&mut self, _: &DependencyAction, step: &str) -> Result<bool> {
            Ok(self.applied.contains(&step))
        }

        fn fetch(&mut self, _: &DependencyAction) -> Result<()> {
            self.calls.push("fetch");
            Ok(())
        }
        fn rebase(&mut self, _: &DependencyAction) -> Result<()> {
            self.calls.push("rebase");
            if self.fail == Some("rebase") {
                Err(Error::git_command_failed("conflict"))
            } else {
                Ok(())
            }
        }
        fn push(&mut self, _: &DependencyAction) -> Result<()> {
            self.calls.push("push");
            Ok(())
        }
        fn update_pull_request_base(&mut self, _: &DependencyAction) -> Result<()> {
            self.calls.push("pr");
            if self.fail == Some("pr") {
                Err(Error::git_command_failed("edit failed"))
            } else {
                Ok(())
            }
        }
        fn invalidate_review(&mut self, _: &DependencyAction) -> Result<()> {
            self.calls.push("review");
            Ok(())
        }
    }
    fn metadata() -> Value {
        json!({ "dependency_graph": { "nodes": [{"id":"foundation","depends_on":[]},{"id":"dependent","depends_on":["foundation"],"worktree":"/tmp/dependent","head":"feature/dependent"}]}})
    }
    #[test]
    fn resumes_from_incomplete_invalidation_without_repeating_push() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let id = format!("dependency-steps-{}", uuid::Uuid::new_v4());
        persist_fanout_run_batch(
            &id,
            &id,
            &[FanoutRunBatchChild {
                task_id: "dependent".into(),
                run_id: "cook-dependent".into(),
            }],
            metadata(),
        )
        .unwrap();
        let resolution = DependencyResolution {
            child_id: "foundation".into(),
            upstream_revision: "a".repeat(40),
            target_base: "main".into(),
        };
        let mut first = Fake::default();
        execute_resolved_dependency_actions(&id, &[resolution.clone()], &mut first).unwrap();
        assert_eq!(first.calls, ["fetch", "rebase", "push"]);
        let mut resumed = Fake::default();
        execute_resolved_dependency_actions(&id, &[resolution], &mut resumed).unwrap();
        assert!(!resumed.calls.contains(&"push"));
    }

    #[test]
    fn reconciles_claimed_push_after_crash_without_repeating_it() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let id = format!("dependency-crash-push-{}", uuid::Uuid::new_v4());
        persist_fanout_run_batch(
            &id,
            &id,
            &[FanoutRunBatchChild {
                task_id: "dependent".into(),
                run_id: "missing-run".into(),
            }],
            metadata(),
        )
        .unwrap();
        let resolution = DependencyResolution {
            child_id: "foundation".into(),
            upstream_revision: "a".repeat(40),
            target_base: "main".into(),
        };
        let key = format!(
            "foundation:dependent:{}:{}",
            resolution.upstream_revision, resolution.target_base
        );
        agent_task_batch::record_dependency_action_receipt(
            &id,
            &key,
            json!({
                "status": "running",
                "steps": {
                    "fetch": { "status": "completed" },
                    "rebase": { "status": "completed" },
                    "push": { "status": "claimed" }
                }
            }),
        )
        .unwrap();

        let mut resumed = Fake {
            applied: vec!["push"],
            ..Default::default()
        };
        execute_resolved_dependency_actions(&id, &[resolution], &mut resumed).unwrap();

        assert!(!resumed.calls.contains(&"push"));
        let receipt = agent_task_batch::dependency_action_receipt(&id, &key)
            .unwrap()
            .unwrap();
        assert_eq!(receipt["steps"]["push"]["status"], "completed");
        assert_eq!(receipt["steps"]["push"]["reconciled"], true);
    }

    #[test]
    fn reconciles_claimed_pr_edit_after_crash_without_repeating_it() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let id = format!("dependency-crash-pr-{}", uuid::Uuid::new_v4());
        crate::agent_task_lifecycle::submit_plan(
            &crate::agent_task_schedule::AgentTaskPlan::new("dependent", Vec::new()),
            Some("dependent-run"),
        )
        .unwrap();
        crate::agent_task_lifecycle::record_cook_finalization(
            "dependent-run",
            json!({ "pr_url": "https://example.test/pr/1" }),
        )
        .unwrap();
        persist_fanout_run_batch(
            &id,
            &id,
            &[FanoutRunBatchChild {
                task_id: "dependent".into(),
                run_id: "dependent-run".into(),
            }],
            metadata(),
        )
        .unwrap();
        let resolution = DependencyResolution {
            child_id: "foundation".into(),
            upstream_revision: "a".repeat(40),
            target_base: "main".into(),
        };
        let key = format!(
            "foundation:dependent:{}:{}",
            resolution.upstream_revision, resolution.target_base
        );
        agent_task_batch::record_dependency_action_receipt(
            &id,
            &key,
            json!({
                "status": "running",
                "steps": {
                    "fetch": { "status": "completed" },
                    "rebase": { "status": "completed" },
                    "push": { "status": "completed" },
                    "pull_request_base_update": { "status": "claimed" }
                }
            }),
        )
        .unwrap();

        let mut resumed = Fake {
            applied: vec!["pull_request_base_update"],
            ..Default::default()
        };
        execute_resolved_dependency_actions(&id, &[resolution], &mut resumed).unwrap();

        assert!(!resumed.calls.contains(&"pr"));
        let receipt = agent_task_batch::dependency_action_receipt(&id, &key)
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt["steps"]["pull_request_base_update"]["status"],
            "completed"
        );
        assert_eq!(
            receipt["steps"]["pull_request_base_update"]["reconciled"],
            true
        );
    }

    #[test]
    fn reconciles_blocked_push_after_the_remote_applied_it() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let id = format!("dependency-blocked-push-{}", uuid::Uuid::new_v4());
        persist_fanout_run_batch(
            &id,
            &id,
            &[FanoutRunBatchChild {
                task_id: "dependent".into(),
                run_id: "missing-run".into(),
            }],
            metadata(),
        )
        .unwrap();
        let resolution = DependencyResolution {
            child_id: "foundation".into(),
            upstream_revision: "a".repeat(40),
            target_base: "main".into(),
        };
        let key = format!(
            "foundation:dependent:{}:{}",
            resolution.upstream_revision, resolution.target_base
        );
        agent_task_batch::record_dependency_action_receipt(
            &id,
            &key,
            json!({
                "status": "blocked",
                "steps": {
                    "fetch": { "status": "completed" },
                    "rebase": { "status": "completed" },
                    "push": { "status": "blocked", "error": "connection lost" }
                }
            }),
        )
        .unwrap();

        let mut resumed = Fake {
            applied: vec!["push"],
            ..Default::default()
        };
        execute_resolved_dependency_actions(&id, &[resolution], &mut resumed).unwrap();

        assert!(!resumed.calls.contains(&"push"));
        let receipt = agent_task_batch::dependency_action_receipt(&id, &key)
            .unwrap()
            .unwrap();
        assert_eq!(receipt["steps"]["push"]["status"], "completed");
        assert_eq!(receipt["steps"]["push"]["reconciled"], true);
    }

    #[test]
    fn repeated_local_invalidation_preserves_the_original_finalization() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        crate::agent_task_lifecycle::submit_plan(
            &crate::agent_task_schedule::AgentTaskPlan::new("dependent", Vec::new()),
            Some("dependent-run"),
        )
        .unwrap();
        crate::agent_task_lifecycle::record_promotion(
            "dependent-run",
            json!({ "status": "succeeded" }),
        )
        .unwrap();
        let finalization =
            json!({ "status": "review_ready", "pr_url": "https://example.test/pr/1" });
        crate::agent_task_lifecycle::record_cook_finalization(
            "dependent-run",
            finalization.clone(),
        )
        .unwrap();

        crate::agent_task_lifecycle::invalidate_cook_finalization_for_dependency(
            "dependent-run",
            "a".repeat(40).as_str(),
            "resume",
        )
        .unwrap();
        let first = crate::agent_task_lifecycle::persisted_status("dependent-run").unwrap();
        crate::agent_task_lifecycle::invalidate_cook_finalization_for_dependency(
            "dependent-run",
            "a".repeat(40).as_str(),
            "resume",
        )
        .unwrap();
        let second = crate::agent_task_lifecycle::persisted_status("dependent-run").unwrap();

        assert_eq!(second.updated_at, first.updated_at);
        assert_eq!(
            second.metadata["cook_recovery_source_checkpoint"]["prior_finalization"],
            finalization
        );
    }
}
