#![cfg(test)]

use super::*;
use crate::{
    agent_task::{
        AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest,
        AGENT_TASK_ARTIFACT_SCHEMA,
    },
    agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateTotals, AgentTaskPlan, AgentTaskProgressEvent,
        AgentTaskQueueStatus, AgentTaskState, AGENT_TASK_AGGREGATE_SCHEMA,
    },
};
use homeboy_core::run_lifecycle_record::{
    ArtifactRetentionLifecycle, ArtifactRetentionStatus, CleanupLifecycle, CleanupState,
    ExternalRuntimeId, FinalizationLifecycle, FinalizationState, ProviderRuntimeLifecycle,
    ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
};
use std::process::Command;

/// Finalization reads `options.run_id`'s durable lifecycle record from the
/// ambient store. Without its own isolated home a test resolves that store
/// through whichever parallel test holds the process-wide home override, and
/// reads it while that test creates or deletes it ("no such table: runs",
/// #14832). Mock-backend tests therefore finalize in their own isolated home;
/// tests that seed durable records isolate once and call
/// `super::finalize_pr_with_backend` directly.
fn finalize_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
) -> Result<AgentTaskPrFinalizationReport> {
    homeboy_core::test_support::with_isolated_home(|_| {
        super::finalize_pr_with_backend(options, backend, &ambient_store()?)
    })
}

/// [`finalize_pr_with_backend`]'s isolation for the preflight entry point.
fn preflight_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
) -> Result<AgentTaskPrFinalizationReport> {
    homeboy_core::test_support::with_isolated_home(|_| {
        super::preflight_pr_with_backend(options, backend, &ambient_store()?)
    })
}

fn ambient_store() -> Result<crate::agent_task_lifecycle::AgentTaskLifecycleStore> {
    crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
}

#[derive(Default)]
pub(super) struct MockBackend {
    pub(super) branch: String,
    changed_files: Vec<String>,
    resolved_base: Option<AgentTaskPrResolvedBase>,
    publication_base_sha: Option<String>,
    candidate_base_sha: Option<String>,
    pr_lookup_complete: bool,
    publication_observed_after_pr_lookup: bool,
    pub(super) existing_pr: Option<AgentTaskPrRef>,
    existing_title: Option<String>,
    last_title: Option<String>,
    merged_pr: Option<AgentTaskPrRef>,
    merged_pr_head_sha: Option<String>,
    create_error: bool,
    /// A PR a concurrent publication opens between our lookup and create:
    /// `create_pr` reports GitHub's conflict and the PR becomes findable.
    concurrent_pr: Option<AgentTaskPrRef>,
    push_error: bool,
    identity_error: bool,
    hydrate_error: bool,
    hydrate_run_id: Option<String>,
    lifecycle: Option<RunLifecycleRecord>,
    gate_proof: Option<AgentTaskPrDurableGateProof>,
    committed: bool,
    pushed: bool,
    pub(super) created: bool,
    created_draft: bool,
    pub(super) create_calls: u8,
    commit_calls: u8,
    push_calls: u8,
    pushed_commit_sha: Option<String>,
    updated: bool,
    last_body: String,
    publication_binding: Option<AgentTaskPublicationBinding>,
    publication_binding_calls: u8,
    binding_io_failures: u8,
    component_ids: Vec<Option<String>>,
    /// Classify the candidate against the real worktree, so a
    /// test observes exactly what finalization would do to the destination.
    pub(super) real_git: bool,
    pub(super) read_failure: bool,
}

impl AgentTaskPrFinalizationBackend for MockBackend {
    fn prepare_manual_candidate(
        &mut self,
        _path: &str,
        base: &str,
    ) -> Result<(crate::agent_task::AgentTaskCandidate, Vec<String>)> {
        if self.real_git {
            return RealAgentTaskPrFinalizationBackend.prepare_manual_candidate(_path, base);
        }
        Ok((
            crate::agent_task::AgentTaskCandidate {
                base_sha: base.into(),
                candidate_sha: "candidate-sha".into(),
                tree_sha: "candidate-tree".into(),
            },
            self.changed_files.clone(),
        ))
    }
    fn candidate_dates(&mut self, _path: &str, _candidate_sha: &str) -> Result<(String, String)> {
        if self.real_git {
            return RealAgentTaskPrFinalizationBackend.candidate_dates(_path, _candidate_sha);
        }
        Ok(("2026-01-01T00:00:00Z".into(), "2026-01-01T00:00:00Z".into()))
    }
    fn publish_tree(
        &mut self,
        _path: &str,
        _tree: &str,
        _parent: &str,
        _message: &str,
        _dates: &(String, String),
    ) -> Result<String> {
        if self.real_git {
            return RealAgentTaskPrFinalizationBackend
                .publish_tree(_path, _tree, _parent, _message, _dates);
        }
        self.committed = true;
        self.commit_calls += 1;
        self.candidate_base_sha = Some(_parent.to_string());
        Ok("candidate-sha".into())
    }

    fn hydrate_run_in_store(
        &mut self,
        lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
        _run_id: &str,
    ) -> Result<RunLifecycleRecord> {
        if self.hydrate_error {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "durable run was not found",
                None,
                None,
            ));
        }
        if let Some(run_id) = self.hydrate_run_id.as_deref() {
            return RealAgentTaskPrFinalizationBackend
                .hydrate_run_in_store(lifecycle_store, run_id);
        }
        Ok(self.lifecycle.clone().unwrap_or_default())
    }
    fn hydrate_gate_proof_in_store(
        &mut self,
        _lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
        _run_id: &str,
    ) -> Result<AgentTaskPrDurableGateProof> {
        self.gate_proof.clone().ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "normal finalization requires durable deterministic gate proof",
                None,
                None,
            )
        })
    }
    fn hydrate_optional_gate_proof_in_store(
        &mut self,
        _lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
        _run_id: &str,
    ) -> Result<Option<AgentTaskPrDurableGateProof>> {
        Ok(self.gate_proof.clone())
    }
    fn current_branch(&mut self, _path: &str) -> Result<String> {
        Ok(if self.branch.is_empty() {
            "fix/cook".to_string()
        } else {
            self.branch.clone()
        })
    }

    fn resolve_base(&mut self, _path: &str, base: &str) -> Result<AgentTaskPrResolvedBase> {
        Ok(self
            .resolved_base
            .clone()
            .unwrap_or_else(|| AgentTaskPrResolvedBase {
                reference: base.to_string(),
                sha: String::new(),
            }))
    }

    fn resolve_verified_base(
        &mut self,
        _path: &str,
        verified_base_sha: &str,
    ) -> Result<AgentTaskPrResolvedBase> {
        Ok(self
            .resolved_base
            .clone()
            .unwrap_or_else(|| AgentTaskPrResolvedBase {
                reference: verified_base_sha.to_string(),
                sha: verified_base_sha.to_string(),
            }))
    }

    fn publication_base_sha(&mut self, _path: &str, _base: &str) -> Result<Option<String>> {
        self.publication_observed_after_pr_lookup = self.pr_lookup_complete;
        Ok(self.publication_base_sha.clone())
    }

    fn validate_publication_identity(
        &mut self,
        _path: &str,
    ) -> Result<homeboy_core::git::GitIdentityProof> {
        if self.identity_error {
            return Err(Error::validation_invalid_argument(
                "git_identity",
                "effective repository-local Git identity does not match the origin host policy",
                None,
                Some(vec!["configure_repository_local_identity".to_string()]),
            ));
        }
        Ok(homeboy_core::git::GitIdentityProof {
            host: "git.example.test".to_string(),
            name: "Homeboy Bot".to_string(),
            email: "bot@example.test".to_string(),
            committer_name: "Homeboy Bot".to_string(),
            committer_email: "bot@example.test".to_string(),
            commit_sha: None,
            scope: "repository_local".to_string(),
        })
    }

    fn push_branch(
        &mut self,
        _path: &str,
        commit_sha: &str,
        head: &str,
    ) -> Result<AgentTaskPublicationGitTracking> {
        if self.real_git {
            return RealAgentTaskPrFinalizationBackend.push_branch(_path, commit_sha, head);
        }
        if self.push_error {
            return Err(Error::git_command_failed("git push failed"));
        }
        self.pushed = true;
        self.push_calls += 1;
        self.pushed_commit_sha = Some(commit_sha.to_string());
        Ok(AgentTaskPublicationGitTracking {
            local_branch: head.to_string(),
            remote: "origin".to_string(),
            upstream_ref: format!("refs/remotes/origin/{head}"),
            verified_remote_sha: "candidate-sha".to_string(),
        })
    }

    fn find_open_pr(
        &mut self,
        _component_id: Option<&str>,
        _path: &str,
        _base: &str,
        _head: &str,
    ) -> Result<Option<AgentTaskPrRef>> {
        self.component_ids.push(_component_id.map(str::to_string));
        self.pr_lookup_complete = true;
        Ok(self.existing_pr.clone())
    }

    fn find_merged_pr(
        &mut self,
        _component_id: Option<&str>,
        _path: &str,
        _base: &str,
        _head: &str,
    ) -> Result<Option<AgentTaskPrRef>> {
        self.component_ids.push(_component_id.map(str::to_string));
        Ok(self.merged_pr.clone())
    }

    fn open_pr_title(
        &mut self,
        _component_id: Option<&str>,
        _path: &str,
        _number: u64,
    ) -> Result<Option<String>> {
        self.component_ids.push(_component_id.map(str::to_string));
        Ok(self.existing_title.clone())
    }

    fn create_pr(
        &mut self,
        _component_id: Option<&str>,
        _path: &str,
        _base: &str,
        _head: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<AgentTaskPrRef> {
        self.component_ids.push(_component_id.map(str::to_string));
        if self.create_error {
            return Err(Error::git_command_failed("gh pr create failed"));
        }
        if let Some(concurrent) = self.concurrent_pr.take() {
            let url = concurrent.url.clone();
            self.existing_pr = Some(concurrent);
            return Err(Error::git_command_failed(format!(
                "gh pr failed: a pull request for branch \"{_head}\" into branch \"{_base}\" already exists:\n{url}"
            )));
        }
        self.created = true;
        self.last_title = Some(title.to_string());
        self.created_draft = draft;
        self.create_calls += 1;
        self.last_body = body.to_string();
        Ok(AgentTaskPrRef {
            number: 123,
            url: "https://github.com/Extra-Chill/homeboy/pull/123".to_string(),
            is_draft: draft,
        })
    }

    fn update_pr(
        &mut self,
        _component_id: Option<&str>,
        _path: &str,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        self.component_ids.push(_component_id.map(str::to_string));
        self.updated = true;
        self.last_title = Some(title.to_string());
        self.last_body = body.to_string();
        Ok(AgentTaskPrRef {
            number,
            url: format!("https://github.com/Extra-Chill/homeboy/pull/{}", number),
            is_draft: false,
        })
    }

    fn verify_publication_binding(
        &mut self,
        _path: &str,
        _base: &str,
        _head: &str,
        candidate_sha: &str,
        changed_files: &[String],
        _pr: &AgentTaskPrRef,
    ) -> Result<AgentTaskPublicationBinding> {
        self.publication_binding_calls += 1;
        if self.read_failure {
            return Err(Error::git_command_failed("gh read unavailable"));
        }
        if self.binding_io_failures > 0 {
            self.binding_io_failures -= 1;
            return Err(Error::internal_io(
                "read temporarily unavailable",
                Some("binding config read".into()),
            ));
        }
        Ok(self
            .publication_binding
            .clone()
            .unwrap_or_else(|| AgentTaskPublicationBinding {
                candidate_sha: candidate_sha.to_string(),
                candidate_tree: "candidate-tree".to_string(),
                remote_sha: candidate_sha.to_string(),
                pr_head_sha: self
                    .merged_pr
                    .as_ref()
                    .filter(|merged| merged.number == _pr.number)
                    .and(self.merged_pr_head_sha.clone())
                    .unwrap_or_else(|| candidate_sha.to_string()),
                repository: "Extra-Chill/homeboy".to_string(),
                head_repository: "Extra-Chill/homeboy".to_string(),
                changed_files: changed_files.to_vec(),
            }))
    }

    fn mark_pr_ready(&mut self, _path: &str, pr: &AgentTaskPrRef) -> Result<AgentTaskPrRef> {
        let mut ready = pr.clone();
        ready.is_draft = false;
        Ok(ready)
    }
}

#[test]
fn publishes_with_accepted_inherited_gate_without_policy_opt_in() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };
    let mut options = options();

    let mut inherited = options.normalized_gate_results[0].clone();
    inherited.id = "baseline-check".to_string();
    inherited.name = "cargo test baseline_check".to_string();
    inherited.status = HomeboyGateStatus::AcceptedInheritedFailure;
    options.normalized_gate_results.push(inherited);

    let report = finalize_pr_with_backend(options, &mut backend).expect("publish non-regression");
    assert_eq!(report.status, "review_ready");
    assert!(backend.created);
    let inherited = report
        .normalized_gate_results
        .iter()
        .find(|gate| gate.id == "baseline-check")
        .expect("inherited evidence retained");
    assert_eq!(
        inherited.status,
        HomeboyGateStatus::AcceptedInheritedFailure
    );
    assert_eq!(inherited.name, "cargo test baseline_check");
}

#[test]
fn creates_new_pr_after_green_gates() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };

    let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

    assert_eq!(report.status, "review_ready");
    assert_eq!(report.pr_action, "created");
    assert_eq!(report.pr_number, Some(123));
    assert!(backend.committed);
    assert!(backend.pushed);
    assert!(backend.created);
    assert_eq!(backend.commit_calls, 1);
    assert_eq!(backend.push_calls, 1);
    assert_eq!(backend.pushed_commit_sha.as_deref(), Some("candidate-sha"));
    assert!(backend.last_body.contains("## Summary"));
    assert!(backend.last_body.contains("## Verification"));
    assert!(backend
        .last_body
        .contains("- `cargo test agent_task_finalization`; expect passes."));
    assert_eq!(backend.last_body.matches("Model: GPT-5.5").count(), 1);
    assert!(backend.last_body.contains("## How to test"));
    assert!(!backend.last_body.contains("## AI assistance"));
    assert!(!backend.last_body.contains("Publication intent"));
    assert_eq!(
        report.publication_intent.schema,
        AGENT_TASK_PUBLICATION_INTENT_SCHEMA
    );
    assert_eq!(report.publication_intent.action, "review_request");
    assert_eq!(report.publication_intent.target.kind, "code_review");
    assert_eq!(
        report.publication_intent.target.adapter.as_deref(),
        Some("github_pull_request")
    );
    assert_eq!(
        report.publication_proof.schema,
        AGENT_TASK_PUBLICATION_PROOF_SCHEMA
    );
    assert_eq!(
        report.publication_proof.adapter_action.as_deref(),
        Some("created")
    );
    assert_eq!(
        report.publication_proof.adapter_ref.as_deref(),
        Some("https://github.com/Extra-Chill/homeboy/pull/123")
    );
    assert_eq!(
        report.finalization_outcome.schema,
        AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA
    );
    assert_eq!(report.finalization_outcome.status, "review_ready");
    assert_eq!(
        report.finalization_outcome.publication_status,
        "review_ready"
    );
    assert_eq!(report.finalization_outcome.publication_action, "created");
    assert_eq!(report.finalization_outcome.base, "main");
    assert_eq!(report.finalization_outcome.head, "fix/cook");
    assert_eq!(report.finalization_outcome.pr_number, Some(123));
    assert_eq!(
        report.finalization_outcome.pr_url.as_deref(),
        Some("https://github.com/Extra-Chill/homeboy/pull/123")
    );
    assert_eq!(
        report.finalization_outcome.changed_files,
        vec!["src/lib.rs"]
    );
    assert!(report.finalization_outcome.committed);
    assert!(report.finalization_outcome.pushed);
    assert!(report.finalization_outcome.published);
    assert_eq!(
        report
            .publication_proof
            .git_identity
            .as_ref()
            .map(|proof| proof.host.as_str()),
        Some("git.example.test")
    );
    assert_eq!(
        report
            .publication_proof
            .git_tracking
            .as_ref()
            .map(|tracking| tracking.upstream_ref.as_str()),
        Some("refs/remotes/origin/fix/cook")
    );
    assert_eq!(
        report
            .publication_proof
            .git_identity
            .as_ref()
            .and_then(|proof| proof.commit_sha.as_deref()),
        Some("candidate-sha")
    );
    assert_eq!(backend.publication_binding_calls, 1);
    assert_eq!(
        report
            .publication_proof
            .binding
            .as_ref()
            .map(|binding| binding.pr_head_sha.as_str()),
        Some("candidate-sha")
    );
}

#[test]
fn adopts_a_pr_a_concurrent_publication_created_after_the_lookup() {
    // #15482: the lookup misses a seconds-old PR, so `gh pr create` reports
    // that it already exists. Publication adopts it instead of failing.
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        concurrent_pr: Some(AgentTaskPrRef {
            number: 683,
            url: "https://github.com/Extra-Chill/homeboy/pull/683".to_string(),
            is_draft: false,
        }),
        ..Default::default()
    };

    let report = finalize_pr_with_backend(options(), &mut backend).expect("adopted");

    assert_eq!(report.status, "review_ready");
    assert_eq!(report.pr_action, "updated");
    assert_eq!(report.pr_number, Some(683));
    assert!(backend.updated);
    assert!(!backend.created);
    assert_eq!(backend.publication_binding_calls, 1);
}

#[test]
fn other_create_failures_still_fail_publication() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        create_error: true,
        ..Default::default()
    };

    let error = finalize_pr_with_backend(options(), &mut backend).expect_err("create fails");
    assert!(error.message.contains("gh pr create failed"));
    assert!(!backend.updated);
}

#[test]
fn cook_form_title_fresh_and_existing_pr_precedence() {
    let form_title = "Require a specific Cook review title";
    let mut fresh = MockBackend {
        changed_files: vec!["src/lib.rs".into()],
        ..Default::default()
    };
    let mut request = options();
    request.title = form_title.into();
    request.cook_form_title = Some(form_title.into());
    request.draft_pr = true;
    finalize_pr_with_backend(request.clone(), &mut fresh).unwrap();
    assert_eq!(fresh.last_title.as_deref(), Some(form_title));

    for (existing_title, expected) in [
        ("Cook homeboy", form_title),
        (
            "Operator-edited specific title",
            "Operator-edited specific title",
        ),
    ] {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".into()],
            existing_pr: Some(AgentTaskPrRef {
                number: 77,
                url: "https://github.com/Extra-Chill/homeboy/pull/77".into(),
                is_draft: true,
            }),
            existing_title: Some(existing_title.into()),
            ..Default::default()
        };
        finalize_pr_with_backend(request.clone(), &mut backend).unwrap();
        assert_eq!(backend.last_title.as_deref(), Some(expected));
    }

    request.title = "Explicit --title".into();
    request.cook_form_title = None;
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".into()],
        existing_pr: Some(AgentTaskPrRef {
            number: 77,
            url: "https://github.com/Extra-Chill/homeboy/pull/77".into(),
            is_draft: true,
        }),
        existing_title: Some("Operator-edited specific title".into()),
        ..Default::default()
    };
    finalize_pr_with_backend(request, &mut backend).unwrap();
    assert_eq!(backend.last_title.as_deref(), Some("Explicit --title"));
}

#[test]
fn preflight_validates_without_publishing_or_git_tracking() {
    // #9798 / #9802: the validation-only (`!publish`) finalization path performs
    // no commit/push, so its publication proof must record no git tracking. This
    // guards the report(...) call that regressed to a missing git_tracking arg
    // and locks the "validated" projection's contract.
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };

    let report = preflight_pr_with_backend(options(), &mut backend).expect("preflight validated");

    assert_eq!(report.status, "validated");
    assert_eq!(report.pr_action, "none");
    assert_eq!(report.pr_number, None);
    assert!(report.pr_url.is_none());

    // No publication mutation happened.
    assert!(!backend.committed);
    assert!(!backend.pushed);
    assert!(!backend.created);
    assert_eq!(backend.commit_calls, 0);
    assert_eq!(backend.push_calls, 0);
    assert!(!report.finalization_outcome.published);

    // Identity is still proven (validation resolves it), but there is no
    // publication git tracking because nothing was pushed.
    assert!(report.publication_proof.git_identity.is_some());
    assert!(
        report.publication_proof.git_tracking.is_none(),
        "validation-only publication proof must not carry git tracking"
    );
}

#[test]
fn finalization_rejects_identity_mismatch_before_any_publication_mutation() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        identity_error: true,
        ..Default::default()
    };

    let error = finalize_pr_with_backend(options(), &mut backend).expect_err("identity mismatch");

    assert!(error.message.contains("repository-local Git identity"));
    assert!(!backend.committed);
    assert!(!backend.pushed);
    assert!(!backend.created);
}

#[test]
fn finalization_pins_verified_base_when_base_advances_before_publication() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        resolved_base: Some(AgentTaskPrResolvedBase {
            reference: "refs/homeboy/finalization/base/main".to_string(),
            sha: "verified-base".to_string(),
        }),
        publication_base_sha: Some("advanced-base".to_string()),
        ..Default::default()
    };

    let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

    assert_eq!(backend.candidate_base_sha.as_deref(), Some("verified-base"));
    assert!(backend.created);
    assert!(backend.publication_observed_after_pr_lookup);
    assert_eq!(
        report
            .publication_intent
            .target
            .verified_base_sha
            .as_deref(),
        Some("verified-base")
    );
    assert_eq!(
        report
            .publication_intent
            .target
            .publication_base_sha
            .as_deref(),
        Some("advanced-base")
    );
    assert!(
        backend
            .last_body
            .contains("Base `main`@`verified-base` advanced to `advanced-base`."),
        "{}",
        backend.last_body
    );
}

#[test]
fn finalization_records_unchanged_live_base_before_publication() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        resolved_base: Some(AgentTaskPrResolvedBase {
            reference: "refs/homeboy/finalization/base/main".to_string(),
            sha: "verified-base".to_string(),
        }),
        publication_base_sha: Some("verified-base".to_string()),
        ..Default::default()
    };

    let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

    assert!(backend.publication_observed_after_pr_lookup);
    assert_eq!(
        report
            .publication_intent
            .target
            .verified_base_sha
            .as_deref(),
        Some("verified-base")
    );
    assert_eq!(
        report
            .publication_intent
            .target
            .publication_base_sha
            .as_deref(),
        Some("verified-base")
    );
    assert!(
        backend
            .last_body
            .contains("Base `main`@`verified-base` unchanged."),
        "{}",
        backend.last_body
    );
    assert!(!backend.last_body.contains("advanced"));
}

#[test]
fn finalization_reports_unavailable_live_base_observation() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        resolved_base: Some(AgentTaskPrResolvedBase {
            reference: "refs/homeboy/finalization/base/main".to_string(),
            sha: "verified-base".to_string(),
        }),
        ..Default::default()
    };

    let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

    assert!(backend.publication_observed_after_pr_lookup);
    assert_eq!(report.publication_intent.target.publication_base_sha, None);
    assert!(backend
        .last_body
        .contains("Base `main`@`verified-base` (publication state not recorded)."));
    assert_eq!(
        report
            .review_dossier
            .verified_base
            .as_ref()
            .and_then(|base| base.publication_sha.as_deref()),
        None
    );
}

#[test]
fn finalization_requires_an_explicit_verified_base_snapshot() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.verified_base_sha = None;

    let error = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect_err("missing snapshot is rejected");

    assert!(error.message.contains("immutable base SHA"));
    assert!(!backend.created);
}

#[test]
fn pr_body_labels_ci_equivalent_gates() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };
    let mut options = options();
    options.normalized_gate_results = vec![HomeboyGateResult::new(
        "gate-1",
        "required project gate",
        HomeboyGateKind::Command,
        HomeboyGateStatus::Passed,
    )
    .evidence(json!({
        "command": ["sh", "-lc", "project verify"],
        "exit_code": 0,
        "ci_equivalent": true,
    }))];

    finalize_pr_with_backend(options, &mut backend).expect("finalized");

    assert!(backend.last_body.contains("## Verification"));
    assert!(backend.last_body.contains("required project gate"));
}

#[test]
fn pr_body_reports_iterator_evidence_metadata() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };
    let mut options = options();
    options.evidence.source_relationship = AgentTaskPrSourceRelationship {
        related_finding_id: Some("finding-123".to_string()),
        source_packet_id: Some("packet-456".to_string()),
        change_kind: Some("runtime-fix".to_string()),
        supersedes: vec!["https://github.com/org/repo/pull/1".to_string()],
        depends_on: vec!["https://github.com/org/repo/pull/2".to_string()],
    };
    options.evidence.verification = AgentTaskPrVerification {
        targeted_checks_run: vec!["cargo test pr_body".to_string()],
        dependency_hydration: Vec::new(),
        targeted_checks_unavailable: None,
        ci_expected: vec!["Homeboy CI".to_string()],
        manual_reviewer_check: None,
    };
    options.evidence.runtime_guardrails = AgentTaskPrRuntimeGuardrails {
        why_not_broader_than_packet: Some("Preserves class and href gates.".to_string()),
        evidence_discriminators: vec!["class=brand".to_string(), "href=#top".to_string()],
        nearby_contracts_preserved: vec!["is_branded_inline_anchor".to_string()],
    };

    finalize_pr_with_backend(options, &mut backend).expect("finalized");

    assert!(backend.last_body.contains("Model: GPT-5.5"));
    assert!(!backend.last_body.contains("CI expected"));
}

#[test]
fn pr_body_reports_run_lifecycle_evidence() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };
    let mut options = options();
    options.evidence.lifecycle = Some(RunLifecycleRecord {
        execution: RunExecutionLifecycle {
            state: RunExecutionState::Succeeded,
            started_at: None,
            finished_at: Some("2026-06-16T00:00:05Z".to_string()),
            updated_at: Some("2026-06-16T00:00:05Z".to_string()),
        },
        provider_runtime: vec![ProviderRuntimeLifecycle {
            task_id: "task-a".to_string(),
            backend: "sample-runtime".to_string(),
            state: ProviderRuntimeState::Succeeded,
            stream_uri: None,
            external_runtime_ids: vec![ExternalRuntimeId {
                kind: "provider_run_id".to_string(),
                value: "provider-run-123".to_string(),
                provider: Some("sample-runtime".to_string()),
                url: None,
            }],
            metadata: serde_json::Value::Null,
        }],
        external_runtime_ids: vec![ExternalRuntimeId {
            kind: "provider_run_id".to_string(),
            value: "provider-run-123".to_string(),
            provider: Some("sample-runtime".to_string()),
            url: None,
        }],
        cleanup: CleanupLifecycle {
            state: CleanupState::Preserved,
            policy: Some("preserve".to_string()),
            updated_at: None,
        },
        finalization: FinalizationLifecycle {
            state: FinalizationState::Pending,
            updated_at: None,
        },
        artifact_retention: ArtifactRetentionLifecycle {
            status: ArtifactRetentionStatus::Retained,
            policy: Some("retain".to_string()),
            updated_at: None,
        },
        ..RunLifecycleRecord::default()
    });

    finalize_pr_with_backend(options, &mut backend).expect("finalized");

    assert!(!backend.last_body.contains("Run lifecycle"));
}

#[test]
fn updates_existing_pr_for_same_branch() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        existing_pr: Some(AgentTaskPrRef {
            number: 77,
            url: "https://github.com/Extra-Chill/homeboy/pull/77".to_string(),
            is_draft: false,
        }),
        ..Default::default()
    };

    let mut finalization_options = options();
    finalization_options.component_id = Some("admitted-component".to_string());
    let report = finalize_pr_with_backend(finalization_options, &mut backend).expect("finalized");

    assert_eq!(report.status, "review_ready");
    assert_eq!(report.pr_action, "updated");
    assert_eq!(report.pr_number, Some(77));
    assert!(backend.updated);
    assert!(!backend.created);
    assert_eq!(
        backend.component_ids,
        vec![
            Some("admitted-component".to_string()),
            Some("admitted-component".to_string()),
        ]
    );
}

#[test]
fn draft_policy_creates_a_draft_pr_and_reports_draft_published() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };
    let mut finalization = options();
    finalization.draft_pr = true;

    let report = finalize_pr_with_backend(finalization, &mut backend).expect("finalized");

    assert!(backend.created_draft);
    assert_eq!(report.status, "draft_published");
    assert_eq!(report.finalization_outcome.status, "draft_published");
}

#[test]
fn draft_policy_preserves_an_existing_ready_pr() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        existing_pr: Some(AgentTaskPrRef {
            number: 77,
            url: "https://github.com/Extra-Chill/homeboy/pull/77".to_string(),
            is_draft: false,
        }),
        ..Default::default()
    };
    let mut finalization = options();
    finalization.draft_pr = true;

    let report = finalize_pr_with_backend(finalization, &mut backend).expect("finalized");

    assert!(!backend.created);
    assert!(backend.updated);
    assert_eq!(report.status, "review_ready");
}

#[test]
fn draft_policy_reconciles_an_existing_draft_pr() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        existing_pr: Some(AgentTaskPrRef {
            number: 77,
            url: "https://github.com/Extra-Chill/homeboy/pull/77".to_string(),
            is_draft: true,
        }),
        ..Default::default()
    };
    let mut finalization = options();
    finalization.draft_pr = true;

    let report = finalize_pr_with_backend(finalization, &mut backend).expect("finalized");

    assert!(!backend.created);
    assert!(backend.updated);
    assert_eq!(report.status, "draft_published");
}

#[test]
fn reports_no_changes_without_commit_push_or_pr() {
    let mut backend = MockBackend::default();

    let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

    assert_eq!(report.status, "no_changes");
    assert_eq!(report.pr_action, "none");
    assert_eq!(report.finalization_outcome.status, "no_changes");
    assert_eq!(report.finalization_outcome.publication_action, "none");
    assert!(!report.finalization_outcome.committed);
    assert!(!report.finalization_outcome.pushed);
    assert!(!report.finalization_outcome.published);
    assert!(!backend.committed);
    assert!(!backend.pushed);
    assert!(!backend.created);
    assert_eq!(backend.commit_calls, 0);
    assert_eq!(backend.push_calls, 0);
    assert!(report.publication_proof.git_tracking.is_none());
}

#[test]
fn refuses_protected_branch() {
    let mut backend = MockBackend {
        branch: "main".to_string(),
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };

    let error = finalize_pr_with_backend(options(), &mut backend).expect_err("blocked");

    assert!(error.message.contains("protected branch"));
    assert!(!backend.committed);
}

#[test]
fn propagates_pr_creation_failure() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        create_error: true,
        ..Default::default()
    };

    let error = finalize_pr_with_backend(options(), &mut backend).expect_err("failed");

    assert!(error.message.contains("gh pr create failed"));
    assert!(backend.committed);
    assert!(backend.pushed);
}

#[test]
fn refuses_red_gates() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };
    let mut options = options();
    options.gate_results[0].status = "failed".to_string();
    options.normalized_gate_results[0] = HomeboyGateResult::from(options.gate_results[0].clone());

    let error = finalize_pr_with_backend(options, &mut backend).expect_err("blocked");

    assert!(error.message.contains("green gates"));
    assert!(!backend.committed);
}

#[test]
fn requires_a_real_durable_run_unless_manual_mode_is_selected() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        hydrate_error: true,
        ..Default::default()
    };
    let mut options = options();
    options.manual_finalization = false;
    let error = finalize_pr_with_backend(options, &mut backend)
        .expect_err("missing run blocks finalization");
    assert!(error.message.contains("durable run was not found"));
    assert!(!backend.committed);
}

#[test]
fn manual_finalization_cannot_bypass_acceptance_for_an_existing_run_id() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "manual-acceptance-bypass";
        let mut plan = AgentTaskPlan::new("manual-acceptance", Vec::new());
        plan.metadata = json!({
            "acceptance": { "authority": "independent-review", "policy": "release-v1" }
        });
        crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        let mut proof = successful_gate_proof();
        proof.run_id = run_id.to_string();
        proof.promotion.source.run_id = Some(run_id.to_string());
        proof.promotion.provenance = json!({
            "candidate": { "fingerprint": { "head": "candidate" } }
        });
        proof.promotion.verified_base = Some(
            crate::agent_task_promotion::AgentTaskPromotionVerifiedBase {
                base: "main".to_string(),
                sha: "verified-base".to_string(),
            },
        );
        crate::agent_task_lifecycle::record_promotion(
            run_id,
            serde_json::to_value(&proof.promotion).unwrap(),
        )
        .unwrap();

        let mut options = options();
        options.run_id = run_id.to_string();
        options.manual_finalization = true;
        let mut backend = MockBackend {
            lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
            gate_proof: Some(proof),
            ..Default::default()
        };
        let error =
            super::finalize_pr_with_backend(options, &mut backend, &ambient_store().unwrap())
                .unwrap_err();
        assert!(error.message.contains("awaiting_acceptance"));
        assert!(!backend.committed && !backend.pushed && !backend.created);
    });
}

#[test]
fn durable_finalization_requires_successful_provider_run_and_hydrates_model() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
        gate_proof: Some(successful_gate_proof()),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;
    finalization_options.review_dossier.ai_assistance.model = "caller model".to_string();
    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("successful run finalizes");
    assert_eq!(
        report.review_dossier.ai_assistance.model,
        "openai/gpt-5.6-terra"
    );

    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(RunLifecycleRecord::default()),
        ..Default::default()
    };
    let mut queued_options = options();
    queued_options.manual_finalization = false;
    assert!(finalize_pr_with_backend(queued_options, &mut backend).is_err());

    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
        ..Default::default()
    };
    let mut proofless_options = options();
    proofless_options.manual_finalization = false;
    assert!(finalize_pr_with_backend(proofless_options, &mut backend).is_err());

    let mut mismatched = successful_gate_proof();
    mismatched.run_id = "unrelated-successful-run".to_string();
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
        gate_proof: Some(mismatched),
        ..Default::default()
    };
    let mut mismatched_options = options();
    mismatched_options.manual_finalization = false;
    assert!(finalize_pr_with_backend(mismatched_options, &mut backend).is_err());

    let mut failed = successful_lifecycle("openai/gpt-5.6-terra");
    failed.execution.state = RunExecutionState::Failed;
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(failed),
        ..Default::default()
    };
    let mut failed_options = options();
    failed_options.manual_finalization = false;
    assert!(finalize_pr_with_backend(failed_options, &mut backend).is_err());

    let mut manual_options = options();
    manual_options.review_dossier.ai_assistance.model = "unknown".to_string();
    assert!(finalize_pr_with_backend(
        manual_options,
        &mut MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        }
    )
    .is_err());
}

#[test]
fn durable_finalization_discloses_terminal_successful_model_after_same_backend_rotation() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(rotated_lifecycle(
            "pi",
            "openai/gpt-5.6-sol",
            "pi",
            "openai/gpt-5.6-terra",
        )),
        gate_proof: Some(successful_gate_proof()),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;

    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("successful rotated run finalizes");

    assert_eq!(
        report.review_dossier.ai_assistance.model,
        "openai/gpt-5.6-terra"
    );
}

#[test]
fn durable_finalization_discloses_terminal_successful_model_after_cross_backend_rotation() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(rotated_lifecycle(
            "pi",
            "openai/gpt-5.6-sol",
            "claude-code",
            "anthropic/claude-sonnet-4",
        )),
        gate_proof: Some(successful_gate_proof()),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;

    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("successful rotated run finalizes");

    assert_eq!(
        report.review_dossier.ai_assistance.model,
        "anthropic/claude-sonnet-4"
    );
}

#[test]
fn durable_finalization_accepts_fingerprinted_candidate_from_successful_fallback() {
    let mut lifecycle = successful_lifecycle("openai/gpt-5.6-terra");
    lifecycle.provider_runtime = vec![
        ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: "quota-exhausted-provider".to_string(),
            state: ProviderRuntimeState::Failed,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({ "failure": "quota_exhausted", "model": "openai/gpt-5.6-sol" }),
        },
        ProviderRuntimeLifecycle {
            task_id: "account-fallback".to_string(),
            backend: "account-unavailable-provider".to_string(),
            state: ProviderRuntimeState::Failed,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({ "failure": "account_unavailable", "model": "openai/gpt-5.6-sol" }),
        },
        ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: "fallback-provider".to_string(),
            state: ProviderRuntimeState::Succeeded,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({ "model": "openai/gpt-5.6-terra" }),
        },
    ];
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(lifecycle.clone()),
        gate_proof: Some(successful_gate_proof()),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;

    finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("successful fallback candidate finalizes");
    assert_eq!(lifecycle.provider_runtime.len(), 3);
    assert_eq!(
        lifecycle.provider_runtime[0].state,
        ProviderRuntimeState::Failed
    );
    assert_eq!(
        lifecycle.provider_runtime[1].state,
        ProviderRuntimeState::Failed
    );

    let mut failed_only = lifecycle.clone();
    failed_only.provider_runtime[2].state = ProviderRuntimeState::Failed;
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(failed_only),
        gate_proof: Some(successful_gate_proof()),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;
    assert!(finalize_pr_with_backend(finalization_options, &mut backend).is_err());

    let mut unbound_candidate = successful_gate_proof();
    unbound_candidate.promotion.source.task_id = "other-task".to_string();
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(lifecycle),
        gate_proof: Some(unbound_candidate),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;
    assert!(finalize_pr_with_backend(finalization_options, &mut backend).is_err());
}

#[test]
fn finalization_keeps_internal_durable_refs_for_operators_but_not_reviewers() {
    let internal_ref = "homeboy://agent-task/run/run-9568/artifacts#task=cook&artifact=patch";
    let reviewer_ref = "https://github.com/Extra-Chill/homeboy/issues/9568";

    let mut durable_options = options();
    durable_options.manual_finalization = false;
    durable_options.changed_files = vec!["src/lib.rs".to_string()];
    durable_options.evidence.source_refs = vec![reviewer_ref.to_string()];
    durable_options.evidence.artifact_refs =
        vec![internal_ref.to_string(), reviewer_ref.to_string()];
    durable_options.review_dossier.evidence.push(
        crate::agent_task_review_dossier::AgentTaskReviewEvidence {
            summary: "Hydrated durable artifact".to_string(),
            url: Some(internal_ref.to_string()),
        },
    );
    let mut durable_gate_proof = successful_gate_proof();
    durable_gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
    let mut durable_backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
        gate_proof: Some(durable_gate_proof),
        ..Default::default()
    };
    let durable_report = finalize_pr_with_backend(durable_options, &mut durable_backend)
        .expect("durable finalization succeeds");

    let mut manual_options = options();
    manual_options.evidence.source_refs = vec![reviewer_ref.to_string()];
    manual_options.evidence.artifact_refs.clear();
    manual_options.review_dossier.evidence.push(
        crate::agent_task_review_dossier::AgentTaskReviewEvidence {
            summary: "Operator-supplied reviewer evidence".to_string(),
            url: Some(reviewer_ref.to_string()),
        },
    );
    manual_options.review_dossier.evidence.push(
        crate::agent_task_review_dossier::AgentTaskReviewEvidence {
            summary: "Hydrated source-run artifact".to_string(),
            url: Some(internal_ref.to_string()),
        },
    );
    let mut manual_backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };
    let manual_report = finalize_pr_with_backend(manual_options, &mut manual_backend)
        .expect("manual finalization succeeds");

    assert!(durable_report
        .evidence
        .artifact_refs
        .contains(&internal_ref.to_string()));
    // The task source ref is a PR relationship, not a restated evidence bullet.
    assert!(!durable_backend.last_body.contains("source_refs"));
    assert!(durable_report
        .publication_intent
        .artifact_refs
        .contains(&internal_ref.to_string()));
    assert!(manual_report.evidence.artifact_refs.is_empty());
    assert!(manual_report.publication_intent.artifact_refs.is_empty());

    for (report, body) in [
        (&durable_report, &durable_backend.last_body),
        (&manual_report, &manual_backend.last_body),
    ] {
        assert!(report
            .review_dossier
            .evidence
            .iter()
            .any(|evidence| evidence.url.as_deref() == Some(reviewer_ref)));
        assert!(!report.review_dossier.evidence.iter().any(|evidence| {
            evidence
                .url
                .as_deref()
                .is_some_and(|url| url.starts_with("homeboy://"))
        }));
        assert!(body.contains(reviewer_ref));
        assert!(!body.contains(internal_ref));
    }
}

#[test]
fn durable_finalization_accepts_only_authenticated_pre_provider_candidate_adoption_recovery() {
    let recovery_lifecycle = RunLifecycleRecord {
        execution: RunExecutionLifecycle {
            state: RunExecutionState::Cancelled,
            started_at: None,
            finished_at: Some("2026-01-01T00:00:00Z".to_string()),
            updated_at: None,
        },
        ..RunLifecycleRecord::default()
    };
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(recovery_lifecycle.clone()),
        gate_proof: Some({
            let mut proof = pre_provider_adoption_gate_proof();
            proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
            proof
        }),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;
    finalization_options.changed_files = vec!["src/lib.rs".to_string()];
    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("authenticated recovery publishes");
    assert_eq!(report.status, "review_ready");
    assert_eq!(report.review_dossier.ai_assistance.model, "GPT-5.5");
    assert!(backend.committed && backend.pushed && backend.created);

    let mut failed_recovery_lifecycle = recovery_lifecycle.clone();
    failed_recovery_lifecycle.execution.state = RunExecutionState::Failed;
    failed_recovery_lifecycle
        .provider_runtime
        .push(ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: "pi".to_string(),
            state: ProviderRuntimeState::Succeeded,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({ "evidence_source": "canonical_executor_outcome" }),
        });
    let mut failed_backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(failed_recovery_lifecycle.clone()),
        gate_proof: Some({
            let mut proof = pre_provider_adoption_gate_proof();
            proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
            proof
        }),
        ..Default::default()
    };
    let mut failed_finalization_options = options();
    failed_finalization_options.manual_finalization = false;
    failed_finalization_options.changed_files = vec!["src/lib.rs".to_string()];
    let failed_report = finalize_pr_with_backend(failed_finalization_options, &mut failed_backend)
        .expect("authenticated failed transport recovery publishes");
    assert_eq!(failed_report.status, "review_ready");
    assert!(failed_backend.committed && failed_backend.pushed && failed_backend.created);

    let rejected = |lifecycle: RunLifecycleRecord, gate_proof: AgentTaskPrDurableGateProof| {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(lifecycle),
            gate_proof: Some(gate_proof),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.manual_finalization = false;
        let error = finalize_pr_with_backend(finalization_options, &mut backend)
            .expect_err("unsafe recovery is blocked");
        assert!(error.message.contains("publication refused:"));
        assert!(!backend.committed && !backend.pushed && !backend.created);
    };

    rejected(failed_recovery_lifecycle.clone(), successful_gate_proof());

    let mut provider_executed = failed_recovery_lifecycle.clone();
    provider_executed
        .provider_runtime
        .push(ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: "provider".to_string(),
            state: ProviderRuntimeState::Cancelled,
            stream_uri: None,
            external_runtime_ids: vec![ExternalRuntimeId {
                kind: "provider_run_id".to_string(),
                value: "provider-actual-run".to_string(),
                provider: Some("provider".to_string()),
                url: None,
            }],
            metadata: serde_json::Value::Null,
        });
    rejected(provider_executed, pre_provider_adoption_gate_proof());

    let mut legacy = pre_provider_adoption_gate_proof();
    legacy.promotion.provenance["adoption"]["recovery"] = serde_json::Value::Null;
    rejected(failed_recovery_lifecycle.clone(), legacy);

    let mut mismatched = pre_provider_adoption_gate_proof();
    mismatched.promotion.provenance["adoption"]["source_run_id"] = json!("other-run");
    rejected(failed_recovery_lifecycle.clone(), mismatched);

    let mut unbound_candidate = pre_provider_adoption_gate_proof();
    unbound_candidate.promotion.provenance["candidate"] = serde_json::Value::Null;
    rejected(failed_recovery_lifecycle.clone(), unbound_candidate);

    let mut mismatched_head = pre_provider_adoption_gate_proof();
    mismatched_head.promotion.provenance["candidate"]["fingerprint"]["head"] =
        json!("0000000000000000000000000000000000000000");
    rejected(failed_recovery_lifecycle.clone(), mismatched_head);

    let mut missing_head = pre_provider_adoption_gate_proof();
    missing_head.promotion.provenance["candidate"]["fingerprint"]["head"] = json!("");
    rejected(failed_recovery_lifecycle.clone(), missing_head);

    let mut missing_model = pre_provider_adoption_gate_proof();
    missing_model.promotion.provenance["adoption"]["ai_model"] = serde_json::Value::Null;
    rejected(failed_recovery_lifecycle.clone(), missing_model);

    let mut non_green = pre_provider_adoption_gate_proof();
    fail_gate(&mut non_green);
    rejected(failed_recovery_lifecycle, non_green);
}

#[test]
fn durable_finalization_accepts_only_authenticated_external_candidate_adoption() {
    let partial_lifecycle = RunLifecycleRecord {
        execution: RunExecutionLifecycle {
            state: RunExecutionState::PartialFailure,
            started_at: None,
            finished_at: Some("2026-01-01T00:00:00Z".to_string()),
            updated_at: None,
        },
        ..RunLifecycleRecord::default()
    };
    let finalize = |gate_proof: AgentTaskPrDurableGateProof| {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(partial_lifecycle.clone()),
            gate_proof: Some(gate_proof),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.manual_finalization = false;
        finalization_options.changed_files = vec!["src/lib.rs".to_string()];
        let report = finalize_pr_with_backend(finalization_options, &mut backend);
        (report, backend)
    };

    let mut accepted_proof = external_adoption_gate_proof();
    accepted_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
    let (report, backend) = finalize(accepted_proof);
    assert_eq!(
        report.expect("authenticated adoption publishes").status,
        "review_ready"
    );
    assert!(backend.committed && backend.pushed && backend.created);

    let mut abbreviated_proof = external_adoption_gate_proof();
    abbreviated_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
    abbreviated_proof.promotion.provenance["adoption"]["candidate_ref"] = json!("7f76933ef");
    let (report, backend) = finalize(abbreviated_proof);
    assert_eq!(
        report
            .expect("fingerprinted abbreviated candidate publishes")
            .status,
        "review_ready"
    );
    assert!(backend.committed && backend.pushed && backend.created);

    let rejected = |proof: AgentTaskPrDurableGateProof| {
        let (result, backend) = finalize(proof);
        assert!(result.is_err());
        assert!(!backend.committed && !backend.pushed && !backend.created);
    };

    rejected(successful_gate_proof());

    let mut missing_candidate = external_adoption_gate_proof();
    missing_candidate.promotion.provenance["candidate"] = serde_json::Value::Null;
    rejected(missing_candidate);

    let mut mismatched_candidate = external_adoption_gate_proof();
    mismatched_candidate.promotion.provenance["candidate"]["fingerprint"]["head"] =
        json!("0000000000000000000000000000000000000000");
    rejected(mismatched_candidate);

    let mut non_prefix_candidate = external_adoption_gate_proof();
    non_prefix_candidate.promotion.provenance["adoption"]["candidate_ref"] = json!("7f76933e0");
    rejected(non_prefix_candidate);

    let mut missing_model = external_adoption_gate_proof();
    missing_model.promotion.provenance["adoption"]["ai_model"] = serde_json::Value::Null;
    rejected(missing_model);

    let mut mismatched_source = external_adoption_gate_proof();
    mismatched_source.promotion.provenance["adoption"]["source_run_id"] = json!("other-run");
    rejected(mismatched_source);

    let mut missing_commit_binding = external_adoption_gate_proof();
    missing_commit_binding.promotion.provenance["commit_range"] = json!("base..other");
    rejected(missing_commit_binding);

    let mut non_green = external_adoption_gate_proof();
    fail_gate(&mut non_green);
    rejected(non_green);
}

#[test]
fn durable_finalization_publishes_clean_synced_recovered_candidate() {
    let mut gate_proof = successful_gate_proof();
    gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
    let mut backend = MockBackend {
        lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
        gate_proof: Some(gate_proof),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;
    finalization_options.component_id = Some("admitted-component".to_string());
    finalization_options.changed_files = vec!["src/lib.rs".to_string()];

    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("clean synced candidate publishes");

    assert_eq!(report.status, "review_ready");
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert!(backend.committed && backend.pushed);
    assert!(backend.created);
}

#[test]
fn durable_finalization_accepts_succeeded_generic_executor_outcome_once() {
    let mut gate_proof = successful_gate_proof();
    gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(generic_executor_lifecycle(
            ProviderRuntimeState::Succeeded,
            "openai/gpt-5.6-terra",
        )),
        gate_proof: Some(gate_proof),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;
    finalization_options.component_id = Some("admitted-component".to_string());
    finalization_options.changed_files = vec!["src/lib.rs".to_string()];

    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("succeeded generic executor outcome finalizes");

    assert_eq!(report.pr_action, "created");
    assert!(backend.committed && backend.pushed && backend.created);
    assert_eq!(backend.create_calls, 1);
    assert_eq!(
        backend.component_ids,
        vec![
            Some("admitted-component".to_string()),
            Some("admitted-component".to_string()),
        ]
    );
    assert_eq!(
        report.evidence.lifecycle.as_ref().unwrap().provider_runtime[0].metadata["evidence_source"],
        "canonical_executor_outcome"
    );
    assert!(
        report.evidence.lifecycle.as_ref().unwrap().provider_runtime[0]
            .external_runtime_ids
            .is_empty()
    );

    for rejected_state in [ProviderRuntimeState::Failed, ProviderRuntimeState::TimedOut] {
        let mut gate_proof = successful_gate_proof();
        gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
        let mut rejected_backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(generic_executor_lifecycle(
                rejected_state,
                "openai/gpt-5.6-terra",
            )),
            gate_proof: Some(gate_proof),
            ..Default::default()
        };
        let mut rejected_options = options();
        rejected_options.manual_finalization = false;
        rejected_options.changed_files = vec!["src/lib.rs".to_string()];

        assert!(finalize_pr_with_backend(rejected_options, &mut rejected_backend).is_err());
        assert!(!rejected_backend.committed);
    }
}

#[test]
fn durable_finalization_accepts_successful_retry_with_failed_attempt_history() {
    let mut gate_proof = successful_gate_proof();
    gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
    let mut lifecycle = successful_lifecycle("openai/gpt-5.6-terra");
    lifecycle.provider_runtime.insert(
        0,
        ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: "lab".to_string(),
            state: ProviderRuntimeState::Failed,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({
                "evidence_source": "durable_provider_execution",
                "attempt": 0,
                "model": "openai/gpt-5.6-terra"
            }),
        },
    );
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(lifecycle),
        gate_proof: Some(gate_proof),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;
    finalization_options.changed_files = vec!["src/lib.rs".to_string()];

    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("successful canonical retry receipt admits publication");

    assert_eq!(report.status, "review_ready");
    assert!(backend.created);
}

#[test]
fn durable_finalization_rejects_stale_success_when_latest_producer_failed_or_gates_are_red() {
    for gates_green in [true, false] {
        let mut gate_proof = successful_gate_proof();
        gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
        if !gates_green {
            fail_gate(&mut gate_proof);
        }
        let mut lifecycle = successful_lifecycle("openai/gpt-5.6-terra");
        lifecycle.provider_runtime.push(ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: "lab".to_string(),
            state: ProviderRuntimeState::Failed,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({
                "evidence_source": "durable_provider_execution",
                "attempt": 1,
                "model": "openai/gpt-5.6-terra"
            }),
        });
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(lifecycle),
            gate_proof: Some(gate_proof),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.manual_finalization = false;
        finalization_options.changed_files = vec!["src/lib.rs".to_string()];

        assert!(finalize_pr_with_backend(finalization_options, &mut backend).is_err());
        assert!(!backend.created && !backend.committed && !backend.pushed);
    }
}

#[test]
fn durable_finalization_recovers_served_model_with_proven_inherited_gate() {
    for (proven, served_model, refusal) in [
        (true, Some("gpt-6.1-sol"), None),
        (false, Some("gpt-6.1-sol"), Some("gates are not green")),
        (true, None, Some("concrete model missing")),
    ] {
        let mut gate_proof = successful_gate_proof();
        gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
        let mut inherited =
            serde_json::to_value(&gate_proof.promotion.deterministic_gates[0]).unwrap();
        inherited["id"] = json!("inherited");
        inherited["status"] = json!("accepted_inherited_failure");
        inherited["exit_code"] = json!(1);
        if proven {
            inherited["failure_evidence"] = json!({
                "classification": "candidate_code", "summary": "baseline failure",
                "command": "cargo test", "exit_code": 1, "agent_feedback": "inherited"
            });
            inherited["baseline_comparison"] = json!({
                "base_ref": "main", "exit_code": 1, "failure_fingerprint": "same",
                "matches_candidate_failure": true, "result": "baseline_red"
            });
        }
        gate_proof
            .promotion
            .deterministic_gates
            .push(serde_json::from_value(inherited).unwrap());
        let mut normalized = gate_proof.promotion.gate_results[0].clone();
        normalized.id = "inherited".to_string();
        normalized.status = HomeboyGateStatus::AcceptedInheritedFailure;
        gate_proof.promotion.gate_results.push(normalized);
        let mut lifecycle = successful_lifecycle("");
        lifecycle.execution.state = RunExecutionState::CandidateRecoverable;
        lifecycle.provider_runtime[0].state = ProviderRuntimeState::Failed;
        lifecycle.provider_runtime[0].metadata =
            json!({ "served_models": served_model.into_iter().collect::<Vec<_>>() });
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(lifecycle),
            gate_proof: Some(gate_proof),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.manual_finalization = false;
        finalization_options.changed_files = vec!["src/lib.rs".to_string()];
        let result = finalize_pr_with_backend(finalization_options, &mut backend);
        if let Some(reason) = refusal {
            let error = result.expect_err("unsafe recovery refused");
            assert!(error.message.contains(reason), "{}", error.message);
            assert!(!backend.committed && !backend.pushed && !backend.created);
        } else {
            let report = result.expect("proven inherited failure with served model publishes");
            assert_eq!(report.review_dossier.ai_assistance.model, "gpt-6.1-sol");
            assert!(backend.created);
        }
    }
}

#[test]
fn durable_finalization_preserves_authenticated_partial_provider_recovery() {
    let mut gate_proof = successful_gate_proof();
    gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
    let mut lifecycle = successful_lifecycle("openai/gpt-5.6-terra");
    lifecycle.execution.state = RunExecutionState::PartialFailure;
    lifecycle.provider_runtime[0].metadata["evidence_source"] = json!("durable_provider_execution");
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(lifecycle),
        gate_proof: Some(gate_proof),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;
    finalization_options.changed_files = vec!["src/lib.rs".to_string()];

    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("authenticated all-successful provider recovery remains eligible");

    assert_eq!(report.status, "review_ready");
    assert!(backend.created);
}

#[test]
fn durable_finalization_rejects_model_less_terminal_record_without_mutation() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let plan = AgentTaskPlan::new(
            "obsolete-generic-plan",
            vec![durable_task("task", "pi", Some("openai/gpt-5.6-terra"))],
        );
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            totals: AgentTaskAggregateTotals {
                succeeded: 1,
                ..Default::default()
            },
            outcomes: vec![durable_succeeded_outcome("task", serde_json::Value::Null)],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus::default(),
        };
        let record = crate::agent_task_lifecycle::record_completed_run(
            &plan,
            &aggregate,
            Some("model-less-terminal-record"),
        )
        .expect("terminal record");
        crate::agent_task_lifecycle::rewrite_record_for_test(&record.run_id, |record| {
            record.lifecycle.provider_runtime.clear();
        })
        .expect("obsolete record persisted");
        let before = crate::agent_task_lifecycle::reconcile_status(&record.run_id)
            .expect("obsolete record loads");
        assert!(before.lifecycle.provider_runtime.is_empty());

        let mut backend = MockBackend {
            hydrate_run_id: Some(record.run_id.clone()),
            gate_proof: Some(successful_gate_proof()),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.run_id = record.run_id.clone();
        finalization_options.manual_finalization = false;

        let error = super::finalize_pr_with_backend(
            finalization_options,
            &mut backend,
            &ambient_store().unwrap(),
        )
        .expect_err("model-less terminal record is rejected");
        let after = crate::agent_task_lifecycle::reconcile_status(&record.run_id)
            .expect("obsolete record remains readable");

        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(before, after);
        assert!(!backend.committed);
        assert!(!backend.pushed);
        assert!(!backend.created);
    });
}

#[test]
fn durable_finalization_accepts_native_and_generic_evidence_but_omits_skipped_work() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "mixed-executor-evidence";
        let plan = AgentTaskPlan::new(
            "mixed-executor-plan",
            vec![
                durable_task("native", "native-executor", Some("native-model")),
                durable_task("generic", "pi", Some("openai/gpt-5.6-terra")),
                durable_task("skipped", "pi", None),
            ],
        );
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            totals: AgentTaskAggregateTotals {
                succeeded: 2,
                skipped: 1,
                ..Default::default()
            },
            outcomes: vec![
                durable_succeeded_outcome(
                    "native",
                    json!({
                        "provider": "native-executor",
                        "provider_run_id": "native-run-123",
                        "model": "native-model",
                    }),
                ),
                durable_succeeded_outcome("generic", serde_json::Value::Null),
            ],
            events: vec![AgentTaskProgressEvent {
                task_id: "skipped".to_string(),
                state: AgentTaskState::Skipped,
                attempt: 1,
                message: Some("not applicable".to_string()),
            }],
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus::default(),
        };
        let record =
            crate::agent_task_lifecycle::record_completed_run(&plan, &aggregate, Some(run_id))
                .expect("durable aggregate recorded");
        let runtimes = &record.lifecycle.provider_runtime;

        assert_eq!(
            record.state(),
            crate::agent_task_lifecycle::AgentTaskRunState::Succeeded
        );
        assert_eq!(runtimes.len(), 2);
        assert_eq!(runtimes[0].external_runtime_ids[0].value, "native-run-123");
        assert_eq!(runtimes[1].backend, "pi");
        assert_eq!(
            runtimes[1].metadata["evidence_source"],
            "canonical_executor_outcome"
        );
        assert!(runtimes.iter().all(|runtime| runtime.task_id != "skipped"));
        assert_eq!(record.artifact_refs[0].kind, "patch");

        let mut gate_proof = successful_gate_proof();
        gate_proof.run_id = run_id.to_string();
        gate_proof.promotion.source.run_id = Some(run_id.to_string());
        gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(record.lifecycle),
            gate_proof: Some(gate_proof),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.run_id = run_id.to_string();
        finalization_options.manual_finalization = false;
        finalization_options.changed_files = vec!["src/lib.rs".to_string()];

        let report = super::finalize_pr_with_backend(
            finalization_options.clone(),
            &mut backend,
            &ambient_store().unwrap(),
        )
        .expect("mixed durable evidence finalizes");
        assert_eq!(report.pr_action, "created");
        assert_eq!(backend.create_calls, 1);

        backend.existing_pr = Some(AgentTaskPrRef {
            number: 123,
            url: "https://github.com/Extra-Chill/homeboy/pull/123".to_string(),
            is_draft: false,
        });
        let repeated = super::finalize_pr_with_backend(
            finalization_options,
            &mut backend,
            &ambient_store().unwrap(),
        )
        .expect("existing PR is reused");
        assert_eq!(repeated.pr_action, "updated");
        assert_eq!(backend.create_calls, 1);
        assert!(backend.updated);
    });
}

fn real_git_finalization_options(
    path: &std::path::Path,
    changed_files: Vec<String>,
) -> AgentTaskPrFinalizationOptions {
    let mut options = options();
    options.path = path.display().to_string();
    options.manual_finalization = false;
    options.changed_files = changed_files;
    options.evidence.source_refs =
        vec!["https://github.com/Extra-Chill/homeboy/issues/8058".to_string()];
    options
}

fn real_git_backend(
    path: &std::path::Path,
    candidate: crate::agent_task_promotion::AgentTaskPromotionCandidate,
) -> MockBackend {
    let mut gate_proof = successful_gate_proof();
    gate_proof.promotion.target.path = Some(path.display().to_string());
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } = &candidate
    else {
        unreachable!("test candidate is Git")
    };
    gate_proof.promotion.changed_files = fingerprint.changed_files.clone();
    MockBackend {
        lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
        gate_proof: Some(gate_proof),
        ..Default::default()
    }
}

fn real_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        ["init"].as_slice(),
        ["config", "user.email", "test@example.com"].as_slice(),
        ["config", "user.name", "Test"].as_slice(),
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(dir.path().join("base"), "base").unwrap();
    assert!(Command::new("git")
        .args(["add", "base"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "base"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    dir
}

/// #15473: the promotion records the live remote base as `verified_base`, so a
/// destination created before the base advanced holds its uncommitted
/// candidate on an older ancestor. Finalization used to fast-forward that
/// destination beneath the dirty candidate and then reject its own move as
/// "candidate changed after promotion". It must leave HEAD on the recorded
/// parent and commit the gate-verified candidate there.

/// #15017: a Cook delivering into an existing PR branch holds its candidate
/// uncommitted on a HEAD that is ahead of base, and the base moved during the
/// run in the same files. Finalization used to merge the base into the dirty
/// tree, git refused, and the gate-green candidate was stranded. It must
/// commit the candidate on its verified parent and push, attempting no merge.

/// Real drift is still refused: a candidate whose content changed after
/// promotion on a lagging parent keeps the exact-fingerprint rejection.

#[test]
fn changed_file_order_and_duplicates_are_normalized() {
    let repo = real_git_repo();
    std::fs::write(repo.path().join("a"), "a").unwrap();
    std::fs::write(repo.path().join("b"), "b").unwrap();
    let candidate =
        crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap()).unwrap();
    let mut backend = real_git_backend(repo.path(), candidate);
    let report = finalize_pr_with_backend(
        real_git_finalization_options(
            repo.path(),
            vec!["b".to_string(), "a".to_string(), "a".to_string()],
        ),
        &mut backend,
    )
    .expect("normalized changed files");
    assert_eq!(report.changed_files, vec!["a", "b"]);
    let mut mismatch = real_git_backend(
        repo.path(),
        crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap()).unwrap(),
    );
    assert!(finalize_pr_with_backend(
        real_git_finalization_options(repo.path(), vec!["a".to_string()]),
        &mut mismatch
    )
    .is_err());
}

fn successful_lifecycle(model: &str) -> RunLifecycleRecord {
    RunLifecycleRecord {
        execution: RunExecutionLifecycle {
            state: RunExecutionState::Succeeded,
            started_at: None,
            finished_at: Some("2026-01-01T00:00:00Z".to_string()),
            updated_at: None,
        },
        provider_runtime: vec![ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: "provider".to_string(),
            state: ProviderRuntimeState::Succeeded,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({ "model": model }),
        }],
        ..RunLifecycleRecord::default()
    }
}

fn rotated_lifecycle(
    attempted_backend: &str,
    attempted_model: &str,
    producing_backend: &str,
    producing_model: &str,
) -> RunLifecycleRecord {
    let mut lifecycle = successful_lifecycle(producing_model);
    lifecycle.provider_runtime = vec![
        ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: attempted_backend.to_string(),
            state: ProviderRuntimeState::Succeeded,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({ "model": attempted_model }),
        },
        ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: producing_backend.to_string(),
            state: ProviderRuntimeState::Succeeded,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({ "model": producing_model }),
        },
    ];
    lifecycle
}

fn generic_executor_lifecycle(state: ProviderRuntimeState, model: &str) -> RunLifecycleRecord {
    RunLifecycleRecord {
        execution: RunExecutionLifecycle {
            state: RunExecutionState::Succeeded,
            started_at: None,
            finished_at: Some("2026-01-01T00:00:00Z".to_string()),
            updated_at: None,
        },
        provider_runtime: vec![ProviderRuntimeLifecycle {
            task_id: "task".to_string(),
            backend: "pi".to_string(),
            state,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({
                "evidence_source": "canonical_executor_outcome",
                "executor": { "backend": "pi", "model": model },
                "model": model,
            }),
        }],
        ..RunLifecycleRecord::default()
    }
}

fn durable_task(task_id: &str, backend: &str, model: Option<&str>) -> AgentTaskRequest {
    serde_json::from_value(json!({
        "task_id": task_id,
        "executor": { "backend": backend, "model": model },
        "instructions": "run",
    }))
    .expect("durable task")
}

fn durable_succeeded_outcome(task_id: &str, metadata: serde_json::Value) -> AgentTaskOutcome {
    AgentTaskOutcome {
        task_id: task_id.to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: Some("succeeded".to_string()),
        artifacts: vec![AgentTaskArtifact {
            schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: format!("{task_id}-patch"),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some(format!("/tmp/{task_id}.patch")),
            url: None,
            mime: None,
            size_bytes: None,
            sha256: None,
            metadata: serde_json::Value::Null,
        }],
        metadata,
        ..Default::default()
    }
}

fn successful_gate_proof() -> AgentTaskPrDurableGateProof {
    AgentTaskPrDurableGateProof {
        run_id: "cook-3678".to_string(),
        promotion: serde_json::from_value(json!({
            "status": "applied", "source": { "kind": "aggregate", "task_id": "task", "run_id": "cook-3678" },
            "to_worktree": "worktree", "target": { "worktree": "worktree", "path": "/repo" },
            "candidate": { "base_sha": "base", "candidate_sha": "patch", "tree_sha": "tree" },
            "operator_notification": { "status": "completed", "message": "complete" },
            "provenance": {
                "candidate": {
                    "kind": "git",
                    "fingerprint": {
                        "schema": "homeboy/agent-task-candidate-fingerprint/v2",
                        "target_path": "/repo",
                        "head": "7f76933ef002d195ee1cc5bf21069e0f40b1c972",
                        "changed_files": ["src/lib.rs"],
                        "tree": "5f76933ef002d195ee1cc5bf21069e0f40b1c972"
                    }
                }
            },
            "deterministic_gates": [{ "id": "gate", "command": ["cargo", "test"], "status": "succeeded", "exit_code": 0 }],
            "gate_results": [{ "id": "gate", "name": "cargo test", "kind": "command", "status": "passed" }]
        })).expect("promotion proof"),
    }
}

/// Turn the proof's only gate red in its authoritative detailed report and in
/// the normalized result derived from it.
fn fail_gate(proof: &mut AgentTaskPrDurableGateProof) {
    let gate = &mut proof.promotion.deterministic_gates[0];
    gate.status = crate::agent_task_gate::AgentTaskGateStatus::Failed;
    gate.exit_code = 1;
    proof.promotion.gate_results[0].status = HomeboyGateStatus::Failed;
}

fn pre_provider_adoption_gate_proof() -> AgentTaskPrDurableGateProof {
    let mut proof = successful_gate_proof();
    proof.promotion.provenance = json!({
        "candidate": {
            "kind": "git",
            "fingerprint": {
                "head": "7f76933ef002d195ee1cc5bf21069e0f40b1c972"
            }
        },
        "adoption": {
            "source_run_id": "cook-3678",
            "candidate_ref": "7f76933ef002d195ee1cc5bf21069e0f40b1c972",
            "ai_model": "openai/gpt-5.6-sol",
            "recovery": {
                "schema": "homeboy/agent-task-candidate-adoption-recovery/v1",
                "reason": "pre_provider_transport_failure",
                "provider_executions_consumed": 0
            }
        }
    });
    proof
}

fn external_adoption_gate_proof() -> AgentTaskPrDurableGateProof {
    let mut proof = pre_provider_adoption_gate_proof();
    proof.promotion.provenance["adoption"]["recovery"] = serde_json::Value::Null;
    proof.promotion.provenance["change_source"] = json!("local_commits");
    proof.promotion.provenance["commit_range"] =
        json!("base..7f76933ef002d195ee1cc5bf21069e0f40b1c972");
    proof.promotion.provenance["commits"] =
        json!([{"sha": "7f76933ef002d195ee1cc5bf21069e0f40b1c972"}]);
    proof
}

#[test]
fn validates_publication_intent_contract() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        ..Default::default()
    };
    let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

    validate_publication_intent(&report.publication_intent).expect("intent is valid");

    let mut invalid = report.publication_intent;
    invalid.target.head = Some(String::new());
    let error = validate_publication_intent(&invalid).expect_err("missing head rejected");

    assert!(error.message.contains("target head ref"));
}

pub(super) fn options() -> AgentTaskPrFinalizationOptions {
    let gate_results = vec![AgentTaskGateResult {
        name: "focused project check".to_string(),
        status: "passed".to_string(),
        detail: Some("targeted".to_string()),
    }];
    let normalized_gate_results = gate_results
        .iter()
        .cloned()
        .map(HomeboyGateResult::from)
        .collect();

    AgentTaskPrFinalizationOptions {
        path: "/repo".to_string(),
        component_id: None,
        run_id: "cook-3678".to_string(),
        base: "main".to_string(),
        verified_base_sha: Some("verified-base".to_string()),
        head: None,
        title: "Cook issue #3678".to_string(),
        cook_form_title: None,
        commit_message: "finalize cook loop PR plumbing".to_string(),
        gate_results,
        normalized_gate_results,

        changed_files: Vec::new(),
        evidence: AgentTaskPrEvidence {
            source_refs: vec!["https://github.com/Extra-Chill/homeboy/issues/3678".to_string()],
            artifact_refs: vec!["artifact://aggregate.json".to_string()],
            attempt_summary: "attempt 1 passed deterministic gates".to_string(),
            ai_tool: "Pi (GPT-5.5)".to_string(),
            ai_model: Some("GPT-5.5".to_string()),
            source_relationship: AgentTaskPrSourceRelationship::default(),
            verification: AgentTaskPrVerification::default(),
            runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
            changed_public_contracts: Vec::new(),
            public_contract_evidence: None,
            lifecycle: None,
        },
        review_dossier: AgentTaskReviewDossier {
            schema: "homeboy/agent-task-review-dossier/v1".to_string(),
            summary: "Finalize a verified candidate.".to_string(),
            what_changed: vec!["Updates the finalization contract.".to_string()],
            how_to_test: vec![crate::agent_task_review_dossier::AgentTaskReviewTestStep {
                command: "cargo test agent_task_finalization".to_string(),
                expected: "passes".to_string(),
            }],
            evidence: Vec::new(),
            verified_commands: Vec::new(),
            changed_public_contracts: Vec::new(),
            public_contract_evidence: None,
            verified_base: None,
            ai_assistance: crate::agent_task_review_dossier::AgentTaskReviewAiAssistance {
                used: true,
                tool: "Homeboy (Pi (GPT-5.5))".to_string(),
                model: "GPT-5.5".to_string(),
            },
            source_relationships: Vec::new(),
            overrides: Vec::new(),
            out_of_scope_changes: Vec::new(),
            repro: None,
        },
        composed_ai_model_disclosure: false,
        review_profile: crate::agent_task_review_dossier::default_profile(),
        manual_finalization: true,
        candidate: None,
        expected_candidate_tree: None,
        verified_candidate_tree: None,
        inherited_gate_evidence: None,
        protected_branches: vec![
            "main".to_string(),
            "master".to_string(),
            "trunk".to_string(),
        ],
        draft_pr: false,
        repository_integrity_evidence: None,
    }
}

#[test]
fn changed_files_mismatch_error_reports_missing_and_unexpected_paths() {
    // Caller supplied a file the promotion did not, and omitted one it did:
    // both divergences must be named, with accurate counts.
    let expected = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
    let caller = vec!["src/a.rs".to_string(), "src/typo.rs".to_string()];

    let error = changed_files_mismatch_error("agent-task-1", &caller, &expected);
    let message = error.message;

    assert!(message.contains("expected_count=2"), "{message}");
    assert!(message.contains("actual_count=2"), "{message}");
    assert!(
        message.contains("missing_from_caller=[src/b.rs]"),
        "{message}"
    );
    assert!(
        message.contains("unexpected_from_caller=[src/typo.rs]"),
        "{message}"
    );
    assert!(
        message.contains("homeboy agent-task diagnose agent-task-1 --full"),
        "{message}"
    );
}

#[test]
fn changed_files_mismatch_error_flags_stale_base_inflated_promotion() {
    // Stale-base attribution inflates the expected set well beyond the caller's
    // current PR diff; every extra expected path shows as missing_from_caller.
    let caller = vec!["src/a.rs".to_string()];
    let expected: Vec<String> = (0..6).map(|index| format!("stale/f{index}.rs")).collect();

    let error = changed_files_mismatch_error("agent-task-2", &caller, &expected);
    let message = error.message;

    assert!(message.contains("expected_count=6"), "{message}");
    assert!(message.contains("actual_count=1"), "{message}");
    assert!(
        message.contains("unexpected_from_caller=[src/a.rs]"),
        "{message}"
    );
    // The stale base paths are all reported as missing from the caller.
    assert!(message.contains("stale/f0.rs"), "{message}");
}

#[test]
fn changed_files_mismatch_error_bounds_large_path_sets() {
    let caller: Vec<String> = Vec::new();
    let expected: Vec<String> = (0..30).map(|index| format!("dir/f{index:02}.rs")).collect();

    let error = changed_files_mismatch_error("agent-task-3", &caller, &expected);
    let message = error.message;

    assert!(message.contains("expected_count=30"), "{message}");
    // Only the bounded prefix is listed, with an explicit omitted count.
    assert!(message.contains("+10 more"), "{message}");
}
