//! PR readiness and mergeability reasoning: CI-check classification, the
//! merge-readiness interpreter, and the GitHub-vs-local mergeability reconciler.

use std::path::Path;

use crate::core::error::Result;

use super::super::github_types::{
    GithubPrReadinessOutput, PrMergeReadiness, PrMergeabilityGitEvidence,
    PrMergeabilityGithubEvidence, PrMergeabilityReconcileOptions, PrMergeabilityReconcileOutput,
    PrReadinessBlocker,
};
use super::super::{resolve_target, run_git, run_git_output};
use super::pulls::pr_view;

/// Explain whether a PR is ready to merge without attempting a merge.
pub fn pr_readiness(
    component_id: Option<&str>,
    number: u64,
    path: Option<String>,
) -> Result<GithubPrReadinessOutput> {
    let pr = pr_view(component_id, number, path)?;
    let readiness = interpret_pr_merge_readiness(
        pr.merge_state.as_deref(),
        &pr.ci_state,
        &pr.ci_summary,
        pr.review_decision.as_deref(),
        pr.draft,
    );

    Ok(GithubPrReadinessOutput {
        component_id: pr.component_id,
        owner: pr.owner,
        repo: pr.repo,
        action: "pr.readiness".to_string(),
        success: true,
        number: pr.number,
        url: pr.url,
        title: pr.title,
        state: pr.state,
        draft: pr.draft,
        review_decision: pr.review_decision,
        ci_state: pr.ci_state,
        ci_summary: pr.ci_summary,
        readiness,
    })
}

/// Compare GitHub's PR mergeability state with local `git merge-tree` evidence.
pub fn pr_reconcile_mergeability(
    component_id: Option<&str>,
    options: PrMergeabilityReconcileOptions,
) -> Result<PrMergeabilityReconcileOutput> {
    let view = pr_view(component_id, options.number, options.path.clone())?;
    let (_id, repo_path) = resolve_target(component_id, options.path.as_deref())?;
    let repo_path = Path::new(&repo_path);

    let base_ref = format!("origin/{}", view.base);
    let head_ref = format!("pull/{}/head", view.number);
    let base_sha = fetch_ref_sha(repo_path, &view.base)?;
    let head_sha = fetch_ref_sha(repo_path, &head_ref)?;
    let merge_tree = run_git_output(
        repo_path,
        &["merge-tree", "--write-tree", &base_sha, &head_sha],
        "git merge-tree",
    )?;
    let merge_tree_stdout = String::from_utf8_lossy(&merge_tree.stdout)
        .trim()
        .to_string();
    let merge_tree_stderr = String::from_utf8_lossy(&merge_tree.stderr)
        .trim()
        .to_string();
    let merge_tree_clean = merge_tree.status.success();
    let head_matches_github = view
        .head_sha
        .as_ref()
        .map(|github_sha| github_sha.eq_ignore_ascii_case(&head_sha));
    let github_merge_state = view.merge_state.as_deref().unwrap_or_default();
    let (classification, recommended_action) =
        classify_mergeability_reconcile(merge_tree_clean, github_merge_state, head_matches_github);

    Ok(PrMergeabilityReconcileOutput {
        component_id: view.component_id,
        owner: view.owner,
        repo: view.repo,
        action: "pr.reconcile_mergeability".to_string(),
        number: view.number,
        classification: classification.to_string(),
        recommended_action: recommended_action.to_string(),
        github: PrMergeabilityGithubEvidence {
            state: view.state,
            base: view.base,
            head: view.head,
            head_repository: view.head_repository,
            head_sha: view.head_sha,
            merge_state: view.merge_state,
            ci_state: view.ci_state,
            ci_summary: view.ci_summary,
        },
        git: PrMergeabilityGitEvidence {
            base_ref,
            base_sha,
            head_ref,
            head_sha,
            merge_tree_clean,
            merge_tree_exit_code: merge_tree.status.code(),
            merge_tree_stdout,
            merge_tree_stderr,
            head_matches_github,
        },
    })
}

fn fetch_ref_sha(repo_path: &Path, remote_ref: &str) -> Result<String> {
    run_git(
        repo_path,
        &["fetch", "--quiet", "origin", remote_ref],
        "git fetch",
    )?;
    Ok(
        run_git(repo_path, &["rev-parse", "FETCH_HEAD"], "git rev-parse")?
            .trim()
            .to_string(),
    )
}

fn classify_mergeability_reconcile(
    merge_tree_clean: bool,
    github_merge_state: &str,
    head_matches_github: Option<bool>,
) -> (&'static str, &'static str) {
    if head_matches_github == Some(false) {
        return ("github_stale", "wait");
    }
    if !merge_tree_clean {
        return ("real_conflict", "resolve_conflicts");
    }

    match github_merge_state.to_ascii_uppercase().as_str() {
        "CLEAN" | "HAS_HOOKS" | "UNSTABLE" => ("clean", "proceed"),
        "BEHIND" => ("needs_update", "update_branch"),
        "DIRTY" | "UNKNOWN" | "" => ("github_stale", "wait"),
        _ => ("needs_update", "rebase_or_replace"),
    }
}

pub(super) fn classify_pr_ci(
    pr_state: &str,
    merged_at: Option<&str>,
    merge_state: Option<&str>,
    checks: &[serde_json::Value],
) -> (String, String, String) {
    if checks.is_empty() {
        return (
            "no_checks".to_string(),
            "GitHub reported no status checks for this PR head; next action: merge-ready"
                .to_string(),
            "merge_ready".to_string(),
        );
    }

    let mut passed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut queued = 0usize;
    let mut running = 0usize;
    let mut pending = 0usize;
    let mut unknown = 0usize;
    let mut rerunnable = 0usize;
    let mut required = 0usize;
    let mut optional = 0usize;
    let mut failed_details = Vec::new();
    let mut pending_details = Vec::new();

    for check in checks {
        let name = check_name(check);
        let workflow = string_field(check, &["workflowName", "workflow_name"]);
        let status = check
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let conclusion = check
            .get("conclusion")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if let Some(is_required) = bool_field(check, &["isRequired", "required"]) {
            if is_required {
                required += 1;
            } else {
                optional += 1;
            }
        }

        match (status, conclusion) {
            (_, Some("FAILURE" | "ACTION_REQUIRED")) => {
                failed += 1;
                failed_details.push(check_detail(check, &name));
            }
            (_, Some("CANCELLED" | "TIMED_OUT" | "STARTUP_FAILURE")) => {
                failed += 1;
                rerunnable += 1;
                failed_details.push(check_detail(check, &name));
            }
            (Some("COMPLETED"), Some("SUCCESS" | "NEUTRAL")) => {
                passed += 1;
            }
            (Some("COMPLETED"), Some("SKIPPED")) => {
                skipped += 1;
            }
            (Some("COMPLETED"), Some(_)) => {
                unknown += 1;
                failed_details.push(check_detail(check, &name));
            }
            (Some("COMPLETED"), None) => {
                unknown += 1;
                failed_details.push(check_detail(check, &name));
            }
            (Some("QUEUED" | "REQUESTED" | "WAITING"), _) => {
                queued += 1;
                pending_details.push(check_pending_detail(check, &name, workflow.as_deref()));
            }
            (Some("IN_PROGRESS"), _) => {
                running += 1;
                pending_details.push(check_pending_detail(check, &name, workflow.as_deref()));
            }
            _ => {
                pending += 1;
                pending_details.push(check_pending_detail(check, &name, workflow.as_deref()));
            }
        }
    }

    let blocked = failed + unknown;
    let waiting = queued + running + pending;
    let state = if failed > 0 || unknown > 0 {
        "terminal_failed"
    } else if waiting > 0 && (pr_state == "MERGED" || merged_at.is_some()) {
        "stale"
    } else if waiting > 0 {
        "pending"
    } else {
        "terminal_green"
    };

    let next_action = if blocked > 0 && failed == rerunnable && unknown == 0 {
        "rerun"
    } else if blocked > 0 {
        "inspect_failed_logs"
    } else if matches!(merge_state, Some("BEHIND")) {
        "update_branch"
    } else if waiting > 0 {
        "wait"
    } else {
        "merge_ready"
    };

    let mut parts = vec![format!(
        "{} reported check(s): {} passed, {} failed/unknown, {} queued, {} running, {} pending, {} skipped",
        checks.len(), passed, blocked, queued, running, pending, skipped
    )];
    if required > 0 || optional > 0 {
        parts.push(format!("{} required, {} optional", required, optional));
    } else {
        parts.push("required/optional split unavailable".to_string());
    }
    if let Some(oldest) = pending_details
        .iter()
        .filter_map(|detail| detail.started_at.as_deref())
        .min()
    {
        parts.push(format!("oldest pending since {}", oldest));
    }
    if !pending_details.is_empty() {
        parts.push(format!(
            "waiting: {}",
            pending_details
                .iter()
                .take(3)
                .map(PendingCheckDetail::label)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !failed_details.is_empty() {
        parts.push(format!(
            "failed logs: {}",
            failed_details
                .iter()
                .take(3)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let action_label = if next_action == "merge_ready" {
        "merge-ready".to_string()
    } else {
        next_action.replace('_', " ")
    };
    parts.push(format!("next action: {}", action_label));

    (state.to_string(), parts.join("; "), next_action.to_string())
}

struct PendingCheckDetail {
    name: String,
    workflow: Option<String>,
    started_at: Option<String>,
}

impl PendingCheckDetail {
    fn label(&self) -> String {
        match (&self.workflow, &self.started_at) {
            (Some(workflow), Some(started_at)) => {
                format!("{} ({}, since {})", self.name, workflow, started_at)
            }
            (Some(workflow), None) => format!("{} ({})", self.name, workflow),
            (None, Some(started_at)) => format!("{} (since {})", self.name, started_at),
            (None, None) => self.name.clone(),
        }
    }
}

fn check_pending_detail(
    check: &serde_json::Value,
    name: &str,
    workflow: Option<&str>,
) -> PendingCheckDetail {
    PendingCheckDetail {
        name: name.to_string(),
        workflow: workflow.map(str::to_string),
        started_at: string_field(check, &["startedAt", "started_at", "queuedAt", "queued_at"]),
    }
}

fn check_detail(check: &serde_json::Value, name: &str) -> String {
    match string_field(
        check,
        &[
            "detailsUrl",
            "details_url",
            "targetUrl",
            "target_url",
            "url",
        ],
    ) {
        Some(url) => format!("{} ({})", name, url),
        None => name.to_string(),
    }
}

fn check_name(check: &serde_json::Value) -> String {
    string_field(check, &["name", "context", "workflowName", "workflow_name"])
        .unwrap_or_else(|| "unnamed check".to_string())
}

fn string_field(check: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| check.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn bool_field(check: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| check.get(*key).and_then(serde_json::Value::as_bool))
}

fn interpret_pr_merge_readiness(
    raw_merge_state: Option<&str>,
    ci_state: &str,
    ci_summary: &str,
    review_decision: Option<&str>,
    draft: bool,
) -> PrMergeReadiness {
    let normalized_merge_state = raw_merge_state
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_uppercase);
    let raw = normalized_merge_state.as_deref();
    let mut blockers = Vec::new();

    if draft {
        blockers.push(readiness_blocker(
            "draft",
            "PR is still a draft.",
            "Mark the PR ready for review before merging.",
        ));
    }

    if review_decision == Some("REVIEW_REQUIRED") {
        blockers.push(readiness_blocker(
            "review_required",
            "GitHub reports that review is required.",
            "Request or wait for the required approving review.",
        ));
    }

    let (interpreted_state, check_guidance, conflict_guidance) = match raw {
        Some("CLEAN") if ci_state == "terminal_green" => (
            "mergeable_now",
            "Required checks reported success for the current PR head.",
            "No conflict guidance; GitHub reports the PR branch is clean.",
        ),
        Some("CLEAN") if ci_state == "no_checks" => {
            blockers.push(readiness_blocker(
                "checks_not_reported",
                "GitHub reports CLEAN but statusCheckRollup is empty.",
                "Wait for checks to appear on the current head before treating the PR as mergeable.",
            ));
            (
                "waiting_on_required_checks",
                "GitHub has not reported checks for this head yet; this can happen immediately after a push or rebase.",
                "No conflict guidance; GitHub reports the PR branch is clean.",
            )
        }
        Some("CLEAN") | Some("BLOCKED") if ci_state == "pending" => {
            blockers.push(readiness_blocker(
                "required_checks_pending",
                "Required checks are still pending or GitHub has not finished branch-protection evaluation.",
                "Wait for required checks to complete, then re-run readiness.",
            ));
            (
                "waiting_on_required_checks",
                "Wait for pending required checks to complete.",
                "No conflict guidance unless GitHub later reports DIRTY or BEHIND.",
            )
        }
        Some("BLOCKED") if ci_state == "terminal_failed" => {
            blockers.push(readiness_blocker(
                "required_checks_failed",
                "Required checks failed or branch protection blocks merge.",
                "Open the PR checks view, fix failing required checks, then re-run readiness.",
            ));
            (
                "failing_required_checks",
                "Fix failing required checks before merging.",
                "No conflict guidance unless GitHub also reports DIRTY or BEHIND.",
            )
        }
        Some("UNSTABLE") if ci_state == "pending" => {
            blockers.push(readiness_blocker(
                "optional_checks_pending",
                "GitHub reports UNSTABLE while checks are pending.",
                "Wait for optional or non-blocking checks to finish if your workflow requires them.",
            ));
            (
                "waiting_on_optional_checks",
                "The PR may be mergeable by GitHub policy, but optional checks have not settled.",
                "No conflict guidance; UNSTABLE is a check signal, not a conflict signal.",
            )
        }
        Some("UNSTABLE") => {
            blockers.push(readiness_blocker(
                "optional_checks_unstable",
                "GitHub reports UNSTABLE; non-required checks are failing or inconclusive.",
                "Inspect the check run details and decide whether optional failures are acceptable.",
            ));
            (
                "failing_optional_checks",
                "Optional or non-required checks are failing or inconclusive.",
                "No conflict guidance; UNSTABLE is a check signal, not a conflict signal.",
            )
        }
        Some("DIRTY") => {
            blockers.push(readiness_blocker(
                "merge_conflicts",
                "GitHub reports merge conflicts with the base branch.",
                "Rebase or merge the base branch locally, resolve conflicts, push, then re-run readiness.",
            ));
            (
                "conflicted",
                "Check status is secondary until conflicts are resolved.",
                "Resolve merge conflicts against the base branch before merging.",
            )
        }
        Some("BEHIND") => {
            blockers.push(readiness_blocker(
                "branch_behind",
                "The PR branch is behind the base branch and must be updated.",
                "Update the branch from the base branch, push, then re-run readiness.",
            ));
            (
                "conflicted",
                "Checks may need to run again after the branch is updated.",
                "Update the PR branch with the base branch before merging.",
            )
        }
        Some("UNKNOWN") | None => {
            blockers.push(readiness_blocker(
                "mergeability_unknown",
                "GitHub has not computed mergeability for the current PR head yet.",
                "Wait briefly and re-run readiness; do not attempt a merge just to discover state.",
            ));
            (
                "unknown",
                "Check state alone is insufficient while mergeability is UNKNOWN.",
                "Conflict state is unknown until GitHub recomputes mergeability.",
            )
        }
        Some("HAS_HOOKS") => {
            blockers.push(readiness_blocker(
                "merge_hooks",
                "GitHub reports merge hooks must run before mergeability is final.",
                "Wait for repository hooks or branch rules to settle, then re-run readiness.",
            ));
            (
                "unknown",
                "Check state is not enough while merge hooks are pending.",
                "Conflict state is not final until hooks complete.",
            )
        }
        Some("BLOCKED") => {
            blockers.push(readiness_blocker(
                "branch_protection_blocked",
                "GitHub branch protection blocks merge.",
                "Inspect required reviews, required checks, conversations, and branch rules in GitHub.",
            ));
            (
                "failing_required_checks",
                "Branch protection is blocking merge; inspect required checks and rules.",
                "No conflict guidance unless GitHub also reports DIRTY or BEHIND.",
            )
        }
        Some(_) if ci_state == "terminal_failed" => {
            blockers.push(readiness_blocker(
                "checks_failed",
                "One or more checks failed or reported an unknown conclusion.",
                "Open the PR checks view, fix failures, then re-run readiness.",
            ));
            (
                "failing_required_checks",
                "Fix failing checks before merging.",
                "No conflict guidance unless GitHub reports DIRTY or BEHIND.",
            )
        }
        Some(_) if ci_state == "pending" => {
            blockers.push(readiness_blocker(
                "checks_pending",
                "One or more checks are still pending.",
                "Wait for checks to finish, then re-run readiness.",
            ));
            (
                "waiting_on_required_checks",
                "Wait for pending checks to complete.",
                "No conflict guidance unless GitHub reports DIRTY or BEHIND.",
            )
        }
        _ => (
            "unknown",
            "Homeboy does not recognize this merge/check combination yet.",
            "Inspect GitHub's PR merge box and raw mergeStateStatus.",
        ),
    };

    if ci_state == "stale" {
        blockers.push(readiness_blocker(
            "stale_check_rollup",
            "GitHub check rollup appears stale for a merged or recently changed PR.",
            "Refresh GitHub state and re-run readiness before using this as merge evidence.",
        ));
    }

    let interpreted_state = if interpreted_state == "mergeable_now" && !blockers.is_empty() {
        "failing_required_checks"
    } else {
        interpreted_state
    };
    let mergeable = interpreted_state == "mergeable_now" && blockers.is_empty();
    let check_guidance = format!("{} {}", check_guidance, ci_summary)
        .trim()
        .to_string();

    PrMergeReadiness {
        raw_merge_state: normalized_merge_state,
        interpreted_state: interpreted_state.to_string(),
        mergeable,
        blockers,
        check_guidance,
        conflict_guidance: conflict_guidance.to_string(),
    }
}

fn readiness_blocker(kind: &str, message: &str, guidance: &str) -> PrReadinessBlocker {
    PrReadinessBlocker {
        kind: kind.to_string(),
        message: message.to_string(),
        guidance: guidance.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_pr_ci_distinguishes_terminal_green() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"SUCCESS"},
            {"status":"COMPLETED","conclusion":"SKIPPED"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, None, checks.as_array().unwrap());

        assert_eq!(state, "terminal_green");
        assert!(summary.contains("1 passed"));
        assert!(summary.contains("1 skipped"));
        assert_eq!(next_action, "merge_ready");
    }

    #[test]
    fn classify_pr_ci_distinguishes_terminal_failed() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"SUCCESS"},
            {"status":"COMPLETED","conclusion":"FAILURE","name":"homeboy / Test","detailsUrl":"https://example.test/logs"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, None, checks.as_array().unwrap());

        assert_eq!(state, "terminal_failed");
        assert!(summary.contains("1 failed/unknown"));
        assert!(summary.contains("homeboy / Test (https://example.test/logs)"));
        assert_eq!(next_action, "inspect_failed_logs");
    }

    #[test]
    fn classify_pr_ci_distinguishes_pending_open_pr() {
        let checks = serde_json::json!([
            {"status":"QUEUED","conclusion":"","name":"homeboy / Build","workflowName":"CI","startedAt":"2026-06-22T01:00:00Z"},
            {"status":"IN_PROGRESS","conclusion":"","name":"homeboy / Test","workflowName":"CI","startedAt":"2026-06-22T01:01:00Z"},
            {"status":"PENDING","conclusion":"","name":"required/context"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, None, checks.as_array().unwrap());

        assert_eq!(state, "pending");
        assert!(summary.contains("1 queued"));
        assert!(summary.contains("1 running"));
        assert!(summary.contains("1 pending"));
        assert!(summary.contains("oldest pending since 2026-06-22T01:00:00Z"));
        assert!(summary.contains("homeboy / Build (CI, since 2026-06-22T01:00:00Z)"));
        assert_eq!(next_action, "wait");
    }

    #[test]
    fn classify_pr_ci_marks_merged_pending_checks_as_stale() {
        let checks = serde_json::json!([
            {"name":"homeboy / Test","status":"IN_PROGRESS","conclusion":""}
        ]);
        let (state, summary, next_action) = classify_pr_ci(
            "MERGED",
            Some("2026-06-15T12:47:01Z"),
            None,
            checks.as_array().unwrap(),
        );

        assert_eq!(state, "stale");
        assert!(summary.contains("1 running"));
        assert_eq!(next_action, "wait");
    }

    #[test]
    fn classify_pr_ci_recommends_update_branch_when_behind() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"SUCCESS"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, Some("BEHIND"), checks.as_array().unwrap());

        assert_eq!(state, "terminal_green");
        assert!(summary.contains("next action: update branch"));
        assert_eq!(next_action, "update_branch");
    }

    #[test]
    fn classify_pr_ci_recommends_rerun_for_cancelled_checks() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"CANCELLED","name":"homeboy / Lint","detailsUrl":"https://example.test/lint"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, None, checks.as_array().unwrap());

        assert_eq!(state, "terminal_failed");
        assert!(summary.contains("next action: rerun"));
        assert_eq!(next_action, "rerun");
    }

    #[test]
    fn readiness_explains_unknown_without_merge_probe() {
        let readiness = interpret_pr_merge_readiness(
            Some("UNKNOWN"),
            "terminal_green",
            "1 check(s): 1 terminal-green, 0 failed/unknown, 0 pending",
            Some("APPROVED"),
            false,
        );

        assert_eq!(readiness.raw_merge_state.as_deref(), Some("UNKNOWN"));
        assert_eq!(readiness.interpreted_state, "unknown");
        assert!(!readiness.mergeable);
        assert_eq!(readiness.blockers[0].kind, "mergeability_unknown");
        assert!(readiness.blockers[0]
            .guidance
            .contains("do not attempt a merge"));
    }

    #[test]
    fn readiness_explains_unstable_as_optional_checks() {
        let readiness = interpret_pr_merge_readiness(
            Some("UNSTABLE"),
            "terminal_failed",
            "2 check(s): 1 terminal-green, 1 failed/unknown, 0 pending",
            Some("APPROVED"),
            false,
        );

        assert_eq!(readiness.interpreted_state, "failing_optional_checks");
        assert!(!readiness.mergeable);
        assert_eq!(readiness.blockers[0].kind, "optional_checks_unstable");
        assert!(readiness
            .conflict_guidance
            .contains("not a conflict signal"));
    }

    #[test]
    fn readiness_treats_clean_without_checks_as_required_wait() {
        let readiness = interpret_pr_merge_readiness(
            Some("CLEAN"),
            "no_checks",
            "GitHub reported no status checks for this PR head.",
            Some("APPROVED"),
            false,
        );

        assert_eq!(readiness.interpreted_state, "waiting_on_required_checks");
        assert!(!readiness.mergeable);
        assert_eq!(readiness.blockers[0].kind, "checks_not_reported");
    }

    #[test]
    fn readiness_allows_clean_green_non_draft_pr() {
        let readiness = interpret_pr_merge_readiness(
            Some("CLEAN"),
            "terminal_green",
            "1 check(s): 1 terminal-green, 0 failed/unknown, 0 pending",
            Some("APPROVED"),
            false,
        );

        assert_eq!(readiness.interpreted_state, "mergeable_now");
        assert!(readiness.mergeable);
        assert!(readiness.blockers.is_empty());
    }
}
