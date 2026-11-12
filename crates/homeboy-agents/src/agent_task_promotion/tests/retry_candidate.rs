//! #16292 §2: the runtime, not a fabricated artifact list, captures retry work.
use super::*;
use crate::agent_task::*;
use crate::agent_task_lifecycle::AgentTaskRunState;
use crate::agent_task_promotion::promote::promote_with_provider_and_checkpoint_in_observation_store;
use crate::agent_task_scheduler::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Retry(AtomicUsize);
impl AgentTaskExecutorAdapter for Retry {
    fn execute(&self, request: AgentTaskRequest, _: AgentTaskExecutionContext) -> AgentTaskOutcome {
        let root = Path::new(request.workspace.root.as_deref().unwrap());
        let attempt = self.0.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            std::fs::write(root.join("inherited.txt"), "previous uncommitted work\n").unwrap();
        } else {
            assert_eq!(
                std::fs::read_to_string(root.join("inherited.txt")).unwrap(),
                "previous uncommitted work\n"
            );
            std::fs::write(root.join("retry.txt"), "remediation\n").unwrap();
        }
        AgentTaskOutcome {
            task_id: request.task_id,
            status: if attempt == 0 {
                AgentTaskOutcomeStatus::Failed
            } else {
                AgentTaskOutcomeStatus::Succeeded
            },
            failure_classification: (attempt == 0)
                .then_some(AgentTaskFailureClassification::Transient),
            ..Default::default()
        }
    }
}

#[test]
fn runtime_retry_inherits_uncommitted_work_promotes_and_pushes_candidate_sha() {
    let _home = gate_home();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.name", "Test"]);
    git(&root, &["config", "user.email", "test@example.test"]);
    std::fs::write(root.join("base"), "base\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "base"]);
    let request: AgentTaskRequest = serde_json::from_value(serde_json::json!({
        "schema": AGENT_TASK_REQUEST_SCHEMA, "task_id":"retry", "executor":{"backend":"test"},
        "instructions":"change", "workspace":{"root":root}, "inputs":{}
    }))
    .unwrap();
    let mut plan = AgentTaskPlan::new("retry-candidate", vec![request]);
    // This exercises candidate inheritance, not timeout handling. Allow Git
    // materialization to complete even when the full suite saturates the host.
    plan.options.timeout_ms = Some(30_000);
    plan.options.retry.max_attempts = 2;
    plan.options.retry.retryable_failure_classifications =
        vec![AgentTaskFailureClassification::Transient];
    plan.options.execution_budget = AgentTaskExecutionBudget::new(2, 1, 0);
    crate::agent_task_lifecycle::submit_plan(&plan, Some("retry-candidate")).unwrap();
    let aggregate = AgentTaskScheduler::new(Arc::new(Retry(AtomicUsize::new(0))))
        .with_run_id("retry-candidate")
        .run(plan);
    assert_eq!(
        aggregate.status(),
        AgentTaskRunState::Succeeded,
        "{aggregate:#?}"
    );
    let outcome = &aggregate.outcomes[0];
    assert!(outcome
        .artifacts
        .iter()
        .all(|artifact| artifact.kind == "patch"));
    let candidate = outcome.candidate.as_ref().unwrap();
    assert_eq!(git_head(&root, "HEAD"), candidate.base_sha);
    git(&root, &["reset", "--hard", &candidate.base_sha]);
    git(&root, &["clean", "-fd"]);
    let mut options = promotion_options(root.to_str().unwrap());
    options.source = serde_json::to_string(outcome).unwrap();
    options.source_worktree_path = Some(root.clone());
    options.gates.verify = vec!["test -f inherited.txt && test -f retry.txt".into()];
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = homeboy_core::observation::ObservationStore::open_initialized_in_roots(
        &context.path_roots(),
    )
    .unwrap();
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(root.clone()),
        ..Default::default()
    };
    let report = promote_with_provider_and_checkpoint_in_observation_store(
        options,
        &mut provider,
        &mut |_| Ok(()),
        &store,
    )
    .unwrap();
    assert_eq!(
        report.status,
        super::super::AgentTaskPromotionStatus::Applied
    );
    assert_eq!(
        report.candidate.as_ref().unwrap().candidate_sha,
        candidate.candidate_sha
    );
    let mut status_json = serde_json::to_value(&report).unwrap();
    super::super::pin_applied_candidate("retry-cook", &mut status_json).unwrap();
    let reference = status_json["candidate_ref"].as_str().unwrap();
    assert_eq!(reference, "refs/homeboy/candidates/retry-cook");
    assert_eq!(git_head(&root, reference), candidate.candidate_sha);
    assert_eq!(
        git_head(&root, &format!("{reference}^{{tree}}")),
        candidate.tree_sha
    );
    assert_eq!(git_head(&root, "HEAD"), candidate.candidate_sha);
    // The destination and a ref checkout both retain the candidate tree.
    let staged = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("write-tree")
        .output()
        .unwrap();
    assert!(staged.status.success());
    assert_eq!(
        String::from_utf8_lossy(&staged.stdout).trim(),
        candidate.tree_sha
    );
    let checkout = temp.path().join("candidate-checkout");
    git(
        &root,
        &["worktree", "add", checkout.to_str().unwrap(), reference],
    );
    assert_eq!(
        std::fs::read(checkout.join("retry.txt")).unwrap(),
        b"remediation\n"
    );
    let clean = std::process::Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(clean.status.success());
    assert!(clean.stdout.is_empty());
    let remote = temp.path().join("published.git");
    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        &root,
        &[
            "push",
            remote.to_str().unwrap(),
            &format!("{}:refs/heads/review", candidate.candidate_sha),
        ],
    );
    assert_eq!(
        git_head(&remote, "refs/heads/review"),
        candidate.candidate_sha
    );
}
