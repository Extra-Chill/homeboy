//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::promote::{
    normalize_promotion_patch, promote_with_provider, promote_with_provider_in_observation_store,
};
use super::super::types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionNotification, AgentTaskPromotionReport,
    AgentTaskPromotionRequest, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};
use super::*;
use crate::agent_task::{AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA};
use crate::agent_task_gate::{
    AgentTaskGateRevealPolicy, AgentTaskGateVisibility, VerifyGateOptions,
};
use serde_json::Value;
use std::path::Path;

#[test]
fn promotion_uses_verified_controller_projection_for_recovered_runner_aggregate_sources() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let observation_store =
            homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let run_id = "recovered-lab-run";
        let task_id = "implement";
        let artifact_id = "patch";
        record_controller_projection(run_id, task_id, artifact_id, VALID_PATCH);
        let source = recovered_runner_aggregate(
            task_id,
            artifact_id,
            &sha256_hex(VALID_PATCH),
            VALID_PATCH.len(),
        );

        for aggregate_file_source in [false, true] {
            let aggregate_file = tempfile::NamedTempFile::new().expect("aggregate source");
            std::fs::write(aggregate_file.path(), &source).expect("write aggregate source");
            let source_path = aggregate_file_source.then(|| aggregate_file.path().to_path_buf());
            let temp = tempfile::tempdir().expect("promotion tempdir");
            let repo = candidate_destination(temp.path());
            let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
            let mut provider = FakePromotionWorkspaceProvider::default();

            let report = promote_with_provider_in_observation_store(
                AgentTaskPromotionRequest {
                    source: with_candidate(&source, &candidate),
                    source_run_id: Some(run_id.to_string()),
                    source_path,
                    source_worktree_path: None,
                    base_ref: None,
                    task_base_sha: None,
                    candidate_ref: None,
                    to_worktree: repo.display().to_string(),
                    task_id: Some(task_id.to_string()),
                    artifact_id: Some(artifact_id.to_string()),
                    dry_run: false,
                    gates: VerifyGateOptions::default(),
                    provider_invocation: None,
                    repository_integrity_evidence: None,
                },
                &mut provider,
                &observation_store,
            )
            .expect("recovered aggregate promotes from controller projection");

            assert_eq!(report.patch_artifact.id, candidate.candidate_sha);
            assert_eq!(
                std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
                "new\n"
            );
        }
    });
}

/// The promoted bytes are the candidate commit's: replacing the harvested
/// patch artifact cannot change what is promoted (#16303).
#[test]
fn promotion_applies_verified_snapshot_when_source_artifact_is_replaced() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("promotion tempdir");
    let repo = candidate_destination(temp.path());
    let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
    let (source_path, source) = write_patch_source(&temp);
    std::fs::write(
        temp.path().join("changes.patch"),
        VALID_PATCH.replace("+new", "+replaced"),
    )
    .expect("replace source artifact");

    let report = promote_with_provider(
        AgentTaskPromotionRequest {
            source: with_candidate(&source, &candidate),
            source_path: Some(source_path),
            ..promotion_options(&repo.display().to_string())
        },
        &mut FakePromotionWorkspaceProvider::default(),
    )
    .expect("promotion uses the candidate commit");

    assert_eq!(report.patch_artifact.kind, "candidate_commit");
    assert_eq!(
        std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
        "new\n"
    );
}

/// Adopting a revision promotes that commit: no patch artifact exists or is
/// written, and the gates run on a destination materialized from it (#16303).
#[test]
fn adopted_revision_promotes_without_a_patch_artifact_and_gates_on_it() {
    let _home = gate_home();
    let (temp, repo, base, candidate) = adopted_commit_repo();
    let mut provider = FakePromotionWorkspaceProvider {
        run_verify_command: true,
        ..Default::default()
    };
    let gate = format!("test \"$(git rev-parse HEAD)\" = {candidate} && grep -qx candidate lib.rs");
    let options = adopted_commit_options(
        &temp,
        &repo,
        base.clone(),
        candidate.clone(),
        VerifyGateOptions {
            verify: vec![gate],
            ..Default::default()
        },
    );

    let report = promote_with_provider(options, &mut provider).expect("adopted revision promotes");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.patch_artifact.kind, "candidate_commit");
    assert_eq!(report.patch_artifact.id, candidate);
    assert_eq!(
        report.provenance["candidate_commit"]["candidate_sha"],
        candidate
    );
    assert_eq!(
        report.provenance["commit_range"],
        format!("{base}..{candidate}")
    );
    assert_eq!(report.changed_files, vec!["lib.rs"]);
    assert_eq!(provider.verify_calls.len(), 1);
    assert_eq!(
        report.deterministic_gates[0].status,
        crate::agent_task_gate::AgentTaskGateStatus::Succeeded
    );
    assert_eq!(report.provenance["candidate_checkout"]["commit"], candidate);
    let patches = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "patch"))
        .count();
    assert_eq!(patches, 0, "adoption must not write a patch artifact");
}

/// A provider timeout leaves edits behind: harvest records them as a candidate
/// commit, and promotion admits and gates that commit with no patch artifact
/// present (#16303).
#[test]
fn recoverable_candidate_promotes_its_harvested_commit() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let base = git_head(&repo, "HEAD");
    let candidate = harvest_recoverable_candidate(&repo, "timeout-run", "task-1");
    let mut provider = FakePromotionWorkspaceProvider::default();
    let mut options = recoverable_candidate_request(&repo, &candidate, "timeout-run");
    options.gates.verify = vec!["true".to_string()];

    let report = promote_with_provider(options, &mut provider)
        .expect("recoverable candidate commit promotes");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.patch_artifact.kind, "candidate_commit");
    assert_eq!(report.patch_artifact.id, candidate.candidate_sha);
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert_eq!(provider.verify_calls.len(), 1);
    assert_eq!(git_head(&repo, "HEAD"), base);
    assert_eq!(
        std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
        "new\n"
    );
}

/// The producing run, task, base and attempt workspace are checked on the
/// candidate commit itself; a mismatch is a typed admission error and the
/// destination is never touched.
#[test]
fn recoverable_candidate_with_mismatched_provenance_is_refused() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let candidate = harvest_recoverable_candidate(&repo, "other-run", "task-1");
    let mut provider = FakePromotionWorkspaceProvider::default();

    let error = promote_with_provider(
        recoverable_candidate_request(&repo, &candidate, "timeout-run"),
        &mut provider,
    )
    .expect_err("a candidate from another run is refused");

    let expected = super::super::CandidateAdmissionError::ProvenanceMismatch {
        candidate_sha: candidate.candidate_sha.clone(),
        check: "run".to_string(),
        expected: "timeout-run".to_string(),
        actual: Some("other-run".to_string()),
    };
    assert_eq!(error.message, expected.to_string());
    assert_eq!(error.details["reason"], "provenance_mismatch");
    assert_eq!(error.details["check"], "run");
    assert!(provider.verify_calls.is_empty());
    assert_eq!(
        std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
        "old\n"
    );

    // A candidate harvested for another task, or not on the recorded task
    // base, is refused the same way.
    let other_task = harvest_recoverable_candidate(&repo, "timeout-run", "task-2");
    let error = promote_with_provider(
        recoverable_candidate_request(&repo, &other_task, "timeout-run"),
        &mut provider,
    )
    .expect_err("a candidate from another task is refused");
    assert_eq!(error.details["check"], "task");
    let candidate = harvest_recoverable_candidate(&repo, "timeout-run", "task-1");
    let mut options = recoverable_candidate_request(&repo, &candidate, "timeout-run");
    options.task_base_sha = Some(candidate.candidate_sha.clone());
    let error = promote_with_provider(options, &mut provider)
        .expect_err("a candidate off the recorded task base is refused");
    assert_eq!(error.details["check"], "base");
}

/// Harvest predating candidate trailers left no recorded provenance; it fails
/// explicitly instead of being read through a compatibility path.
#[test]
fn recoverable_candidate_without_recorded_provenance_is_refused() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let candidate = harvest_patch_candidate(&repo, VALID_PATCH);

    let error = promote_with_provider(
        recoverable_candidate_request(&repo, &candidate, "timeout-run"),
        &mut FakePromotionWorkspaceProvider::default(),
    )
    .expect_err("an unattributed candidate is refused");

    assert_eq!(error.details["check"], "task");
    assert_eq!(error.details["actual"], Value::Null);
}

#[test]
fn normalize_promotion_patch_strips_lab_sandbox_workspace_prefix() {
    let patch = "diff --git a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n--- a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n+++ b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";

    let normalized = normalize_promotion_patch(patch, "homeboy@promoted-task")
        .expect("sandbox-prefixed patch normalizes");

    assert_eq!(normalized.changed_files, vec!["src/lib.rs"]);
    assert_eq!(
        normalized.content,
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
    );
}

#[test]
fn empty_patch_records_declared_base_candidate_delta_before_running_gates() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "homeboy@example.test"]);
    git(&repo, &["config", "user.name", "Homeboy Test"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("lib.rs"), "base\n").expect("write base");
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-m", "base"]);
    git(
        &repo,
        &["remote", "add", "origin", repo.to_str().expect("repo path")],
    );
    git(&repo, &["checkout", "-b", "fix/candidate"]);
    std::fs::write(repo.join("lib.rs"), "candidate\n").expect("write candidate");
    git(&repo, &["commit", "-am", "candidate"]);
    let revision = git_head(&repo, "HEAD");
    let (source_path, source) = write_empty_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionRequest {
            source,
            source_run_id: Some("run-empty".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: Some("main".to_string()),
            // Cook's inspected revision can equal the existing candidate. The
            // declared base remains the publication authority.
            task_base_sha: Some(revision.clone()),
            candidate_ref: None,
            to_worktree: "repo@candidate".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["public-check".to_string()],
                private_verify: vec!["private-check".to_string()],
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
                ..Default::default()
            },
            provider_invocation: None,
            repository_integrity_evidence: None,
        },
        &mut provider,
    )
    .expect("empty candidate verifies");

    assert_eq!(report.status, AgentTaskPromotionStatus::VerifiedNoChanges);
    assert_eq!(report.target.head.as_deref(), Some(revision.as_str()));
    assert_eq!(report.provenance["verified_revision"], revision);
    assert_eq!(report.changed_files, ["lib.rs"]);
    assert_eq!(
        report
            .verified_base
            .as_ref()
            .expect("declared base is retained")
            .base,
        "main"
    );
    assert_eq!(provider.verify_calls.len(), 2);
    assert_eq!(provider.verify_calls[0].0, repo);
    assert_eq!(provider.verify_calls[1].2, AgentTaskGateVisibility::Private);
    assert_eq!(
        provider.verify_calls[1].3,
        AgentTaskGateRevealPolicy::SummaryOnly
    );
}

/// #15294: an attempt Homeboy proved changed nothing has no candidate. Its
/// promotion is a no-change outcome that runs no gates, and commits already on
/// the reused destination branch are not reported as this attempt's change.
#[test]
fn verified_unchanged_attempt_promotes_as_no_change_without_running_gates() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "homeboy@example.test"]);
    git(&repo, &["config", "user.name", "Homeboy Test"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("lib.rs"), "base\n").expect("write base");
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-m", "base"]);
    git(
        &repo,
        &["remote", "add", "origin", repo.to_str().expect("repo path")],
    );
    // A reused issue branch already carries an earlier wave's commit.
    git(&repo, &["checkout", "-b", "fix/issue"]);
    std::fs::write(repo.join("lib.rs"), "earlier wave\n").expect("write prior commit");
    git(&repo, &["commit", "-am", "earlier wave"]);
    let revision = git_head(&repo, "HEAD");
    let (source_path, _) = write_empty_patch_source(&temp);
    let mut outcome: Value =
        serde_json::from_str(&std::fs::read_to_string(&source_path).expect("read source"))
            .expect("parse source");
    outcome["metadata"] = serde_json::json!({
        crate::agent_task::HOMEBOY_EMPTY_ATTEMPT_VERIFIED_KEY: true,
    });
    let source = outcome.to_string();
    std::fs::write(&source_path, &source).expect("write verified unchanged source");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionRequest {
            source,
            source_run_id: Some("run-unchanged".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: Some("main".to_string()),
            task_base_sha: Some(revision.clone()),
            candidate_ref: None,
            to_worktree: "repo@issue".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["public-check".to_string()],
                ..Default::default()
            },
            provider_invocation: None,
            repository_integrity_evidence: None,
        },
        &mut provider,
    )
    .expect("unchanged attempt reports no change");

    assert_eq!(report.status, AgentTaskPromotionStatus::NoChanges);
    assert!(
        report.changed_files.is_empty(),
        "{:?}",
        report.changed_files
    );
    assert!(report.deterministic_gates.is_empty());
    assert!(provider.verify_calls.is_empty(), "no gate may run");
    assert_eq!(report.target.head.as_deref(), Some(revision.as_str()));
}

/// #15855: the audited-candidate requirement is not weakened for a no_op claim
/// without a patch artifact unless Homeboy verified the checkout unchanged.
#[test]
fn no_op_without_patch_artifact_still_requires_an_audited_candidate_without_verification() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let source_path = temp.path().join("outcome.json");
    let source = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task-1",
        "status": "no_op",
        "summary": "executor finished without changes",
    })
    .to_string();
    std::fs::write(&source_path, &source).expect("write source");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().join("target")),
        ..Default::default()
    };

    let error = promote_with_provider(
        AgentTaskPromotionRequest {
            source,
            source_run_id: Some("run-no-op".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "homeboy@no-op-attempt".to_string(),
            task_id: Some("task-1".to_string()),
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_invocation: None,
            repository_integrity_evidence: None,
        },
        &mut provider,
    )
    .expect_err("unverified no-op stays unaudited");

    assert!(error
        .message
        .contains("no-op promotion requires an audited committed candidate"));
}

#[test]
fn promote_exports_committed_changes_when_patch_artifact_is_empty() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let base = git_head(&repo, "HEAD");
    let attempt = temp.path().join("attempt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fix/committed",
            attempt.to_str().unwrap(),
            &base,
        ],
    );
    std::fs::write(attempt.join("src/lib.rs"), "new\n").expect("write committed change");
    git(&attempt, &["commit", "-qam", "fix: committed change"]);
    let candidate = harvest_worktree_candidate(&attempt, &base);
    let (source_path, source) = write_empty_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider::default();

    let report = promote_with_provider(
        AgentTaskPromotionRequest {
            source: with_candidate(&source, &candidate),
            source_run_id: Some("run-committed".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(attempt.clone()),
            task_base_sha: Some(base.clone()),
            gates: VerifyGateOptions {
                verify: vec!["true".to_string()],
                ..Default::default()
            },
            ..promotion_options(&repo.display().to_string())
        },
        &mut provider,
    )
    .expect("committed changes are promoted");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert_eq!(report.patch_artifact.id, candidate.candidate_sha);
    assert_eq!(
        report.provenance["change_source"].as_str(),
        Some("candidate_commit")
    );
    assert_eq!(provider.verify_calls.len(), 1);
    assert_eq!(provider.verify_calls[0].0, attempt);
    assert_eq!(git_head(&repo, "HEAD"), base);
    assert_eq!(
        std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
        "new\n"
    );
}

#[test]
fn validate_patch_rejects_path_traversal() {
    let patch = "--- a/src/lib.rs\n+++ b/../secret\n@@ -1 +1 @@\n-old\n+new\n";

    let err =
        normalize_promotion_patch(patch, "repo@promoted-task").expect_err("unsafe path rejected");

    assert!(err.message.contains("unsafe patch path"));
}

#[test]
fn promote_dry_run_validates_provider_request_without_applying() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
    let (source_path, source) = write_patch_source(&temp);

    let mut provider = FakePromotionWorkspaceProvider::default();
    let report = promote_with_provider(
        AgentTaskPromotionRequest {
            source: with_candidate(&source, &candidate),
            source_path: Some(source_path),
            dry_run: true,
            ..promotion_options(&repo.display().to_string())
        },
        &mut provider,
    )
    .expect("dry-run promotion report");

    assert_eq!(report.status, AgentTaskPromotionStatus::DryRun);
    assert_eq!(report.source.task_id, "task-1");
    assert_eq!(report.patch_artifact.id, candidate.candidate_sha);
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert_eq!(git_stdout(&repo, &["status", "--porcelain"]), "");
    assert!(report.deterministic_gates.is_empty());
}

#[test]
fn promote_verification_failure_keeps_the_applied_target_recoverable() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree_path = std::fs::canonicalize(candidate_destination(temp.path())).unwrap();
        let candidate = harvest_patch_candidate(&worktree_path, VALID_PATCH);
        let (source_path, source) = write_patch_source(&temp);
        let mut provider = FakePromotionWorkspaceProvider {
            verify_exit_code: 1,
            ..Default::default()
        };

        let report = promote_with_provider(
            AgentTaskPromotionRequest {
                source: with_candidate(&source, &candidate),
                source_run_id: Some("runner-only-run".to_string()),
                source_path: Some(source_path),
                gates: VerifyGateOptions {
                    verify: vec!["false".to_string()],
                    ..Default::default()
                },
                ..promotion_options(&worktree_path.display().to_string())
            },
            &mut provider,
        )
        .expect("verification failure is a recoverable promotion report");

        assert_eq!(report.status, AgentTaskPromotionStatus::GateFailed);
        assert!(report.status.patch_promoted());
        assert_eq!(report.target.path.as_deref(), worktree_path.to_str());
        assert_eq!(provider.verify_calls.len(), 1);
        assert_eq!(report.operator_notification.status, "blocked");
    });
}

#[test]
fn promote_applies_normalized_lab_sandbox_patch_with_fake_workspace_provider() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
    let patch = "diff --git a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n--- a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n+++ b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
    std::fs::write(temp.path().join("changes.patch"), patch).expect("write patch");
    let source_path = temp.path().join("outcome.json");
    let source = with_candidate(
        &serde_json::json!({
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "task-1",
            "status": "succeeded",
            "artifacts": [{
                "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                "id": "patch",
                "kind": "patch",
                "path": "changes.patch",
                "size_bytes": patch.len(),
                "sha256": sha256_hex(patch)
            }]
        })
        .to_string(),
        &candidate,
    );
    let mut provider = FakePromotionWorkspaceProvider::default();

    let report = promote_with_provider(
        AgentTaskPromotionRequest {
            source,
            source_path: Some(source_path),
            ..promotion_options(&repo.display().to_string())
        },
        &mut provider,
    )
    .expect("promote applies the candidate behind a Lab sandbox patch");

    // Changed files come from the commit, never from sandbox patch headers.
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert_eq!(report.patch_artifact.id, candidate.candidate_sha);
    assert_eq!(
        std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
        "new\n"
    );
}

#[test]
fn explicit_candidate_rejections_leave_target_unmodified() {
    let (temp, repo, base, candidate) = adopted_commit_repo();
    let cases = [
        (
            "unresolved",
            "not-a-commit".to_string(),
            base.clone(),
            "not present",
        ),
        (
            "stale",
            candidate.clone(),
            "0000000000000000000000000000000000000000".to_string(),
            "git rev-parse",
        ),
    ];
    for (name, candidate_ref, task_base, expected) in cases {
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.clone()),
            ..Default::default()
        };
        let error = promote_with_provider(
            adopted_commit_options(
                &temp,
                &repo,
                task_base,
                candidate_ref,
                VerifyGateOptions::default(),
            ),
            &mut provider,
        )
        .expect_err(name);
        assert!(
            error.message.contains(expected),
            "{name}: {}",
            error.message
        );
    }
}

#[test]
fn promotion_report_serializes_generic_command_evidence() {
    let _home = gate_home();
    let report = AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status: AgentTaskPromotionStatus::Applied,
        source: AgentTaskPromotionSource {
            kind: "outcome".to_string(),
            task_id: "task-1".to_string(),
            run_id: Some("run-1".to_string()),
            path: None,
        },
        to_worktree: "repo@controlled-worktree".to_string(),
        target: AgentTaskPromotionTarget {
            worktree: "repo@controlled-worktree".to_string(),
            path: Some("/tmp/repo@controlled-worktree".to_string()),
            branch: Some("fix/test".to_string()),
            head: Some("abc123".to_string()),
            dirty: Some(true),
        },
        patch_artifact: AgentTaskPromotionArtifactRef {
            id: "patch".to_string(),
            kind: "patch".to_string(),
            path: "changes.patch".to_string(),
            sha256: None,
        },
        changed_files: vec!["src/lib.rs".to_string()],
        command_evidence: vec![command_report(vec![
            "fake-workspace-provider",
            "apply-patch",
        ])],
        deterministic_gates: Vec::new(),
        gate_results: Vec::new(),
        repository_integrity_evidence: Some(
            homeboy_core::repository_integrity::RepositoryIntegrityEvidence {
                origin: "ssh://example.test/homeboy.git".to_string(),
                symlink_exceptions: Vec::new(),
                sha256: "sha256:admitted".to_string(),
            },
        ),
        verified_base: None,
        provenance: Value::Null,
        operator_notification: AgentTaskPromotionNotification {
            status: "completed".to_string(),
            message: "patch promoted".to_string(),
            resumable_blocker: None,
            next_command: None,
        },
    };

    let value = serde_json::to_value(report).expect("serialize report");
    assert_eq!(
        value["repository_integrity_evidence"]["sha256"],
        "sha256:admitted"
    );

    assert_eq!(
        value["command_evidence"][0]["command"][0].as_str(),
        Some("fake-workspace-provider")
    );
}

/// Harvest captures `git diff --binary --full-index --find-renames`; sections
/// for binary edits, pure renames, empty files and mode changes carry only a
/// `diff --git` header, and each must still name its changed files (#16005).
#[test]
fn normalize_promotion_patch_reads_changed_files_from_header_only_sections() {
    type Change = fn(&Path);
    let cases: [(&str, Change, &[&str]); 5] = [
        (
            "binary edit",
            |repo| std::fs::write(repo.join("image.bin"), [0u8, 1, 2, 255, 0, 9]).unwrap(),
            &["image.bin"],
        ),
        (
            "rename",
            |repo| git(repo, &["mv", "notes.txt", "renamed.txt"]),
            &["notes.txt", "renamed.txt"],
        ),
        (
            "empty file",
            |repo| std::fs::write(repo.join("empty.txt"), "").unwrap(),
            &["empty.txt"],
        ),
        (
            "mode change",
            |repo| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    repo.join("notes.txt"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap()
            },
            &["notes.txt"],
        ),
        (
            "text edit",
            |repo| std::fs::write(repo.join("notes.txt"), "changed\n").unwrap(),
            &["notes.txt"],
        ),
    ];
    for (name, change, expected) in cases {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path();
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "homeboy@example.test"]);
        git(repo, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(repo.join("notes.txt"), "original\n").unwrap();
        std::fs::write(repo.join("image.bin"), [0u8, 1, 2, 3, 0, 5]).unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "base"]);
        change(repo);
        git(repo, &["add", "-A"]);
        let output = Command::new("git")
            .current_dir(repo)
            .args([
                "diff",
                "--cached",
                "--binary",
                "--full-index",
                "--find-renames",
                "HEAD",
            ])
            .output()
            .expect("git diff");
        let patch = String::from_utf8(output.stdout).expect("utf8 patch");
        assert!(patch.starts_with("diff --git "), "{name} produced a patch");

        let normalized = normalize_promotion_patch(&patch, "homeboy@promoted-task")
            .unwrap_or_else(|error| panic!("{name} normalizes: {error:?}"));
        assert_eq!(normalized.changed_files, expected, "{name}");
    }
}
