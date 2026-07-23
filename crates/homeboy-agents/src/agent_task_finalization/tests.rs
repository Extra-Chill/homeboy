#![cfg(test)]

use super::*;
use crate::{
    agent_task::{
        AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest,
        AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
    },
    agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals, AgentTaskPlan,
        AgentTaskProgressEvent, AgentTaskQueueStatus, AgentTaskState, AGENT_TASK_AGGREGATE_SCHEMA,
    },
};
use homeboy_core::run_lifecycle_record::{
    ArtifactRetentionLifecycle, ArtifactRetentionStatus, CleanupLifecycle, CleanupState,
    ExternalRuntimeId, FinalizationLifecycle, FinalizationState, ProviderRuntimeLifecycle,
    ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
};
use std::process::Command;

#[derive(Default)]
struct MockBackend {
    branch: String,
    changed_files: Vec<String>,
    candidate_state: Option<AgentTaskPrCandidateState>,
    resolved_base: Option<AgentTaskPrResolvedBase>,
    publication_base_sha: Option<String>,
    candidate_base_sha: Option<String>,
    pr_lookup_complete: bool,
    publication_observed_after_pr_lookup: bool,
    existing_pr: Option<AgentTaskPrRef>,
    create_error: bool,
    push_error: bool,
    identity_error: bool,
    hydrate_error: bool,
    hydrate_run_id: Option<String>,
    lifecycle: Option<RunLifecycleRecord>,
    gate_proof: Option<AgentTaskPrDurableGateProof>,
    candidate: Option<crate::agent_task_promotion::AgentTaskPromotionCandidate>,
    committed: bool,
    pushed: bool,
    created: bool,
    create_calls: u8,
    changed_files_calls: u8,
    commit_calls: u8,
    push_calls: u8,
    updated: bool,
    last_body: String,
}

impl AgentTaskPrFinalizationBackend for MockBackend {
    fn hydrate_run(&mut self, _run_id: &str) -> Result<RunLifecycleRecord> {
        if self.hydrate_error {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "durable run was not found",
                None,
                None,
            ));
        }
        if let Some(run_id) = self.hydrate_run_id.as_deref() {
            return RealAgentTaskPrFinalizationBackend.hydrate_run(run_id);
        }
        Ok(self.lifecycle.clone().unwrap_or_default())
    }
    fn hydrate_gate_proof(&mut self, _run_id: &str) -> Result<AgentTaskPrDurableGateProof> {
        self.gate_proof.clone().ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "normal finalization requires durable deterministic gate proof",
                None,
                None,
            )
        })
    }
    fn validate_candidate(&mut self, options: &AgentTaskPrFinalizationOptions) -> Result<()> {
        let Some(expected) = self.candidate.as_ref() else {
            return Ok(());
        };
        let actual = crate::agent_task_promotion::candidate_fingerprint(&options.path)?;
        if actual != *expected {
            return Err(Error::validation_invalid_argument(
                "path",
                "candidate changed after promotion; rerun promotion gates before finalization",
                None,
                None,
            ));
        }
        let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint: _ } =
            actual
        else {
            unreachable!("test candidate is Git")
        };
        Ok(())
    }
    fn current_branch(&mut self, _path: &str) -> Result<String> {
        Ok(if self.branch.is_empty() {
            "fix/cook".to_string()
        } else {
            self.branch.clone()
        })
    }

    fn changed_files(&mut self, _path: &str) -> Result<Vec<String>> {
        self.changed_files_calls += 1;
        Ok(self.changed_files.clone())
    }

    fn candidate_state(
        &mut self,
        _path: &str,
        base: &AgentTaskPrResolvedBase,
        _head: &str,
    ) -> Result<AgentTaskPrCandidateState> {
        self.candidate_base_sha = Some(base.sha.clone());
        Ok(self.candidate_state.clone().unwrap_or_else(|| {
            if self.changed_files.is_empty() {
                AgentTaskPrCandidateState::Equivalent
            } else {
                AgentTaskPrCandidateState::Dirty {
                    changed_files: self.changed_files.clone(),
                }
            }
        }))
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
            scope: "repository_local".to_string(),
        })
    }

    fn commit_all(&mut self, _path: &str, _message: &str) -> Result<()> {
        self.committed = true;
        self.commit_calls += 1;
        Ok(())
    }

    fn push_branch(&mut self, _path: &str, head: &str) -> Result<AgentTaskPublicationGitTracking> {
        if self.push_error {
            return Err(Error::git_command_failed("git push failed"));
        }
        self.pushed = true;
        self.push_calls += 1;
        Ok(AgentTaskPublicationGitTracking {
            local_branch: head.to_string(),
            remote: "origin".to_string(),
            upstream_ref: format!("refs/remotes/origin/{head}"),
            verified_remote_sha: "candidate-sha".to_string(),
        })
    }

    fn find_open_pr(
        &mut self,
        _path: &str,
        _base: &str,
        _head: &str,
    ) -> Result<Option<AgentTaskPrRef>> {
        self.pr_lookup_complete = true;
        Ok(self.existing_pr.clone())
    }

    fn create_pr(
        &mut self,
        _path: &str,
        _base: &str,
        _head: &str,
        _title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        if self.create_error {
            return Err(Error::git_command_failed("gh pr create failed"));
        }
        self.created = true;
        self.create_calls += 1;
        self.last_body = body.to_string();
        Ok(AgentTaskPrRef {
            number: 123,
            url: "https://github.com/Extra-Chill/homeboy/pull/123".to_string(),
        })
    }

    fn update_pr(
        &mut self,
        _path: &str,
        number: u64,
        _title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        self.updated = true;
        self.last_body = body.to_string();
        Ok(AgentTaskPrRef {
            number,
            url: format!("https://github.com/Extra-Chill/homeboy/pull/{}", number),
        })
    }
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
    assert_eq!(backend.changed_files_calls, 0);
    assert_eq!(backend.commit_calls, 1);
    assert_eq!(backend.push_calls, 1);
    assert!(backend.last_body.contains("## AI assistance"));
    assert!(backend.last_body.contains("## Summary"));
    assert!(backend.last_body.contains("## How to test"));
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
    assert!(backend
        .last_body
        .contains("Verified finalization base: main at verified-base"));
    assert!(backend
        .last_body
        .contains("Base advanced after verification"));
    assert!(backend
        .last_body
        .contains("publication observed advanced-base"));
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
    assert!(backend
        .last_body
        .contains("Base unchanged since verification: main remains at verified-base."));
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
        .contains("Base observation unavailable immediately before publication"));
}

#[test]
fn finalization_rejects_candidate_behind_pinned_base_before_publication() {
    let mut backend = MockBackend {
        resolved_base: Some(AgentTaskPrResolvedBase {
            reference: "refs/homeboy/finalization/base/main".to_string(),
            sha: "verified-base".to_string(),
        }),
        publication_base_sha: Some("advanced-base".to_string()),
        candidate_state: Some(AgentTaskPrCandidateState::Invalid {
            diagnostic: "HEAD is behind the pinned base".to_string(),
        }),
        ..Default::default()
    };

    let error = finalize_pr_with_backend(options(), &mut backend).expect_err("behind candidate");

    assert!(error.message.contains("behind the pinned base"));
    assert_eq!(backend.candidate_base_sha.as_deref(), Some("verified-base"));
    assert!(!backend.committed);
    assert!(!backend.pushed);
    assert!(!backend.created);
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

    assert!(backend.last_body.contains("## Evidence"));
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

    assert!(backend.last_body.contains("## AI assistance"));
    assert!(backend.last_body.contains("GPT-5.5"));
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
        }),
        ..Default::default()
    };

    let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

    assert_eq!(report.status, "review_ready");
    assert_eq!(report.pr_action, "updated");
    assert_eq!(report.pr_number, Some(77));
    assert!(backend.updated);
    assert!(!backend.created);
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
    assert_eq!(backend.changed_files_calls, 0);
    assert_eq!(backend.commit_calls, 0);
    assert_eq!(backend.push_calls, 0);
}

#[test]
fn publishes_clean_committed_candidate_without_committing() {
    let mut backend = MockBackend {
        candidate_state: Some(AgentTaskPrCandidateState::Committed {
            changed_files: vec!["deleted.rs".to_string(), "renamed.rs".to_string()],
            push_required: true,
        }),
        ..Default::default()
    };

    let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

    assert_eq!(report.pr_action, "created");
    assert_eq!(report.changed_files, vec!["deleted.rs", "renamed.rs"]);
    assert!(!backend.committed);
    assert!(backend.pushed);
    assert!(backend.created);
    assert!(!report.finalization_outcome.committed);
    assert!(report.finalization_outcome.pushed);
}

#[test]
fn already_pushed_clean_committed_candidate_updates_open_pr_once() {
    let mut backend = MockBackend {
        candidate_state: Some(AgentTaskPrCandidateState::Committed {
            changed_files: vec!["src/lib.rs".to_string()],
            push_required: false,
        }),
        existing_pr: Some(AgentTaskPrRef {
            number: 77,
            url: "https://github.com/Extra-Chill/homeboy/pull/77".to_string(),
        }),
        ..Default::default()
    };

    let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

    assert_eq!(report.pr_action, "updated");
    assert!(!backend.committed);
    assert!(!backend.pushed);
    assert!(backend.updated);
    assert!(!backend.created);
}

#[test]
fn synthetic_changed_file_does_not_commit_clean_committed_candidate() {
    let mut backend = MockBackend {
        candidate_state: Some(AgentTaskPrCandidateState::Committed {
            changed_files: vec!["src/lib.rs".to_string()],
            push_required: true,
        }),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.changed_files = vec!["synthetic.rs".to_string()];

    let report = finalize_pr_with_backend(finalization_options, &mut backend).expect("finalized");

    assert_eq!(report.changed_files, vec!["synthetic.rs"]);
    assert!(!backend.committed);
    assert!(backend.pushed);
}

#[test]
fn invalid_base_or_diverged_candidate_stops_before_publication() {
    let mut backend = MockBackend {
        candidate_state: Some(AgentTaskPrCandidateState::Invalid {
            diagnostic: "HEAD is behind requested base `trunk`; rebase first".to_string(),
        }),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.base = "trunk".to_string();

    let error = finalize_pr_with_backend(finalization_options, &mut backend).expect_err("blocked");

    assert!(error.message.contains("behind requested base"));
    assert!(!backend.committed);
    assert!(!backend.pushed);
    assert!(!backend.created);
}

#[test]
fn push_failure_stops_pr_publication_for_clean_committed_candidate() {
    let mut backend = MockBackend {
        candidate_state: Some(AgentTaskPrCandidateState::Committed {
            changed_files: vec!["src/lib.rs".to_string()],
            push_required: true,
        }),
        push_error: true,
        ..Default::default()
    };

    let error = finalize_pr_with_backend(options(), &mut backend).expect_err("push fails");

    assert!(error.message.contains("git push failed"));
    assert!(!backend.created);
    assert!(!backend.updated);
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
fn finalization_keeps_internal_durable_refs_for_operators_but_not_reviewers() {
    let internal_ref = "homeboy://agent-task/run/run-9568/artifacts#task=cook&artifact=patch";
    let reviewer_ref = "https://github.com/Extra-Chill/homeboy/issues/9568";

    let mut durable_options = options();
    durable_options.manual_finalization = false;
    durable_options.changed_files = vec!["src/lib.rs".to_string()];
    durable_options.evidence.source_refs = vec![reviewer_ref.to_string()];
    durable_options.evidence.artifact_refs = vec![internal_ref.to_string()];
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
            backend: "opencode".to_string(),
            state: ProviderRuntimeState::Failed,
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
        assert!(error.message.contains("durable run must have succeeded"));
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
    non_green.promotion.gate_results[0].status = HomeboyGateStatus::Failed;
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
    non_green.promotion.gate_results[0].status = HomeboyGateStatus::Failed;
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
    finalization_options.changed_files = vec!["src/lib.rs".to_string()];

    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("clean synced candidate publishes");

    assert_eq!(report.status, "review_ready");
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert!(!backend.committed && !backend.pushed);
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
    finalization_options.changed_files = vec!["src/lib.rs".to_string()];

    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("succeeded generic executor outcome finalizes");

    assert_eq!(report.pr_action, "created");
    assert!(backend.committed && backend.pushed && backend.created);
    assert_eq!(backend.create_calls, 1);
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
fn durable_finalization_rejects_model_less_terminal_record_without_mutation() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let plan = AgentTaskPlan::new(
            "obsolete-generic-plan",
            vec![durable_task(
                "task",
                "opencode",
                Some("openai/gpt-5.6-terra"),
            )],
        );
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
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
        let before =
            crate::agent_task_lifecycle::status(&record.run_id).expect("obsolete record loads");
        assert!(before.lifecycle.provider_runtime.is_empty());

        let mut backend = MockBackend {
            hydrate_run_id: Some(record.run_id.clone()),
            gate_proof: Some(successful_gate_proof()),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.run_id = record.run_id.clone();
        finalization_options.manual_finalization = false;

        let error = finalize_pr_with_backend(finalization_options, &mut backend)
            .expect_err("model-less terminal record is rejected");
        let after = crate::agent_task_lifecycle::status(&record.run_id)
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
        let plan = AgentTaskPlan::new(
            "mixed-executor-plan",
            vec![
                durable_task("native", "native-executor", Some("native-model")),
                durable_task("generic", "opencode", Some("openai/gpt-5.6-terra")),
                durable_task("skipped", "opencode", None),
            ],
        );
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
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
            crate::agent_task_lifecycle::record_completed_run(&plan, &aggregate, Some("cook-3678"))
                .expect("durable aggregate recorded");
        let runtimes = &record.lifecycle.provider_runtime;

        assert_eq!(
            record.state,
            crate::agent_task_lifecycle::AgentTaskRunState::Succeeded
        );
        assert_eq!(runtimes.len(), 2);
        assert_eq!(runtimes[0].external_runtime_ids[0].value, "native-run-123");
        assert_eq!(runtimes[1].backend, "opencode");
        assert_eq!(
            runtimes[1].metadata["evidence_source"],
            "canonical_executor_outcome"
        );
        assert!(runtimes.iter().all(|runtime| runtime.task_id != "skipped"));
        assert_eq!(record.artifact_refs[0].kind, "patch");

        let mut gate_proof = successful_gate_proof();
        gate_proof.promotion.changed_files = vec!["src/lib.rs".to_string()];
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(record.lifecycle),
            gate_proof: Some(gate_proof),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.manual_finalization = false;
        finalization_options.changed_files = vec!["src/lib.rs".to_string()];

        let report = finalize_pr_with_backend(finalization_options.clone(), &mut backend)
            .expect("mixed durable evidence finalizes");
        assert_eq!(report.pr_action, "created");
        assert_eq!(backend.create_calls, 1);

        backend.existing_pr = Some(AgentTaskPrRef {
            number: 123,
            url: "https://github.com/Extra-Chill/homeboy/pull/123".to_string(),
        });
        let repeated = finalize_pr_with_backend(finalization_options, &mut backend)
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
        candidate: Some(candidate),
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

#[test]
fn stale_candidate_mutation_is_rejected_before_commit_and_push() {
    let repo = real_git_repo();
    std::fs::write(repo.path().join("candidate"), "before").unwrap();
    let candidate =
        crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap()).unwrap();
    std::fs::write(repo.path().join("candidate"), "after").unwrap();
    let mut backend = real_git_backend(repo.path(), candidate);
    let error = finalize_pr_with_backend(
        real_git_finalization_options(repo.path(), vec!["candidate".to_string()]),
        &mut backend,
    )
    .expect_err("stale candidate");
    assert!(error.message.contains("candidate changed"));
    assert!(!backend.committed);
    assert!(!backend.pushed);
}

#[test]
fn head_drift_is_rejected_before_commit_and_push() {
    let repo = real_git_repo();
    std::fs::write(repo.path().join("candidate"), "candidate").unwrap();
    let candidate =
        crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap()).unwrap();
    assert!(Command::new("git")
        .args(["add", "candidate"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "drift"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    let mut backend = real_git_backend(repo.path(), candidate);
    assert!(finalize_pr_with_backend(
        real_git_finalization_options(repo.path(), vec!["candidate".to_string()]),
        &mut backend
    )
    .is_err());
    assert!(!backend.committed);
    assert!(!backend.pushed);
}

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

#[test]
fn production_validator_normalizes_changed_file_order_and_duplicates() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let repo = real_git_repo();
        std::fs::write(repo.path().join("a"), "a").unwrap();
        std::fs::write(repo.path().join("b"), "b").unwrap();
        let candidate =
            crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
                .unwrap();
        let run_id = "production-validator-8058";
        crate::agent_task_lifecycle::submit_plan(
            &crate::agent_task_scheduler::AgentTaskPlan::new("validator", Vec::new()),
            Some(run_id),
        )
        .unwrap();
        let mut promotion = successful_gate_proof().promotion;
        promotion.source.run_id = Some(run_id.to_string());
        promotion.target.path = Some(repo.path().display().to_string());
        promotion.changed_files = vec!["a".to_string(), "b".to_string()];
        promotion.provenance = json!({ "candidate": candidate });
        crate::agent_task_lifecycle::record_promotion(
            run_id,
            serde_json::to_value(promotion).unwrap(),
        )
        .unwrap();
        let mut options = real_git_finalization_options(
            repo.path(),
            vec!["b".to_string(), "a".to_string(), "a".to_string()],
        );
        options.run_id = run_id.to_string();
        validate_real_candidate_fingerprint(&options).unwrap();
        options.changed_files = vec!["a".to_string()];
        assert!(validate_real_candidate_fingerprint(&options).is_err());
    });
}

#[test]
fn durable_finalization_uses_promoted_files_for_clean_committed_candidate() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let repo = real_git_repo();
        std::fs::write(repo.path().join("a"), "a").unwrap();
        std::fs::write(repo.path().join("b"), "b").unwrap();
        assert!(Command::new("git")
            .args(["add", "a", "b"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "promoted candidate"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        let candidate =
            crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
                .unwrap();
        let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } =
            &candidate
        else {
            unreachable!("test repository is Git")
        };
        assert!(fingerprint.changed_files.is_empty());

        let run_id = "clean-committed-candidate";
        crate::agent_task_lifecycle::submit_plan(
            &crate::agent_task_scheduler::AgentTaskPlan::new("validator", Vec::new()),
            Some(run_id),
        )
        .unwrap();
        let mut promotion = successful_gate_proof().promotion;
        promotion.source.run_id = Some(run_id.to_string());
        promotion.target.path = Some(repo.path().display().to_string());
        promotion.changed_files = vec!["a".to_string(), "b".to_string()];
        promotion.provenance = json!({ "candidate": candidate });
        crate::agent_task_lifecycle::record_promotion(
            run_id,
            serde_json::to_value(promotion).unwrap(),
        )
        .unwrap();

        let mut options = real_git_finalization_options(
            repo.path(),
            vec!["b".to_string(), "a".to_string(), "a".to_string()],
        );
        options.run_id = run_id.to_string();
        validate_real_candidate_fingerprint(&options).expect("clean committed candidate accepted");

        options.changed_files = vec!["a".to_string()];
        let error = validate_real_candidate_fingerprint(&options)
            .expect_err("mismatched durable changed files rejected");
        assert!(error.message.contains("persisted promotion report"));
    });
}

#[test]
fn production_validator_accepts_only_the_exact_promoted_recovery_commit() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let repo = real_git_repo();
        std::fs::write(repo.path().join("candidate"), "promoted bytes").unwrap();
        let candidate =
            crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
                .unwrap();
        let run_id = "production-recovery-commit";
        crate::agent_task_lifecycle::submit_plan(
            &crate::agent_task_scheduler::AgentTaskPlan::new("validator", Vec::new()),
            Some(run_id),
        )
        .unwrap();
        let mut promotion = successful_gate_proof().promotion;
        promotion.source.run_id = Some(run_id.to_string());
        promotion.target.path = Some(repo.path().display().to_string());
        promotion.changed_files = vec!["candidate".to_string()];
        promotion.provenance = json!({ "candidate": candidate });
        crate::agent_task_lifecycle::record_promotion(
            run_id,
            serde_json::to_value(promotion).unwrap(),
        )
        .unwrap();

        assert!(Command::new("git")
            .args(["add", "candidate"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "recover promoted candidate"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        let mut options = real_git_finalization_options(repo.path(), vec!["candidate".to_string()]);
        options.run_id = run_id.to_string();
        validate_real_candidate_fingerprint(&options).expect("exact recovery commit accepted");

        let mut missing = options.clone();
        missing.changed_files.clear();
        let error =
            validate_real_candidate_fingerprint(&missing).expect_err("missing paths rejected");
        assert!(error.message.contains("persisted promotion report"));
        let mut synthetic = options.clone();
        synthetic.changed_files = vec!["candidate".to_string(), "synthetic".to_string()];
        let error =
            validate_real_candidate_fingerprint(&synthetic).expect_err("extra paths rejected");
        assert!(error.message.contains("persisted promotion report"));

        std::fs::write(repo.path().join("drift"), "drift").unwrap();
        Command::new("git")
            .args(["add", "drift"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "drift"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        let error = validate_real_candidate_fingerprint(&options).expect_err("drift rejected");
        assert!(error.message.contains("parent and tree exactly match"));
    });
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
            backend: "opencode".to_string(),
            state,
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({
                "evidence_source": "canonical_executor_outcome",
                "executor": { "backend": "opencode", "model": model },
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
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: task_id.to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: Some("succeeded".to_string()),
        failure_classification: None,
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
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: serde_json::Value::Null,
        workflow: None,
        follow_up: None,
        metadata,
    }
}

fn successful_gate_proof() -> AgentTaskPrDurableGateProof {
    AgentTaskPrDurableGateProof {
        run_id: "cook-3678".to_string(),
        promotion: serde_json::from_value(json!({
            "status": "applied", "source": { "kind": "aggregate", "task_id": "task", "run_id": "cook-3678" },
            "to_worktree": "worktree", "target": { "worktree": "worktree", "path": "/repo" },
            "patch_artifact": { "id": "patch", "kind": "patch", "path": "patch" },
            "operator_notification": { "status": "completed", "message": "complete" },
            "gate_results": [{ "id": "gate", "name": "cargo test", "kind": "command", "status": "passed" }]
        })).expect("promotion proof"),
    }
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

fn options() -> AgentTaskPrFinalizationOptions {
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
        run_id: "cook-3678".to_string(),
        base: "main".to_string(),
        verified_base_sha: Some("verified-base".to_string()),
        head: None,
        title: "Cook issue #3678".to_string(),
        commit_message: "finalize cook loop PR plumbing".to_string(),
        gate_results,
        normalized_gate_results,
        changed_files: Vec::new(),
        evidence: AgentTaskPrEvidence {
            source_refs: vec!["https://github.com/Extra-Chill/homeboy/issues/3678".to_string()],
            artifact_refs: vec!["artifact://aggregate.json".to_string()],
            attempt_summary: "attempt 1 passed deterministic gates".to_string(),
            ai_tool: "OpenCode (GPT-5.5)".to_string(),
            ai_model: Some("GPT-5.5".to_string()),
            source_relationship: AgentTaskPrSourceRelationship::default(),
            verification: AgentTaskPrVerification::default(),
            runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
            changed_public_contracts: Vec::new(),
            public_contract_evidence: None,
            lifecycle: None,
        },
        ai_used_for: "Traced the finalization contract change, implemented it, and confirmed with the recorded gate."
            .to_string(),
        review_dossier: AgentTaskReviewDossier {
            schema: "homeboy/agent-task-review-dossier/v1".to_string(),
            summary: "Finalize a verified candidate.".to_string(),
            what_changed: vec!["Updates the finalization contract.".to_string()],
            how_to_test: vec![crate::agent_task_review_dossier::AgentTaskReviewTestStep {
                command: "cargo test agent_task_finalization".to_string(),
                expected: "passes".to_string(),
            }],
            compatibility: "No compatibility impact.".to_string(),
            evidence: Vec::new(),
            changed_public_contracts: Vec::new(),
            public_contract_evidence: None,
            ai_assistance: crate::agent_task_review_dossier::AgentTaskReviewAiAssistance {
                used: true,
                tool: "Homeboy (OpenCode (GPT-5.5))".to_string(),
                model: "GPT-5.5".to_string(),
                used_for: "Traced the finalization contract change, implemented it, and confirmed with the recorded gate."
                    .to_string(),
            },
            source_relationships: Vec::new(),
            overrides: Vec::new(),
        },
        review_profile: crate::agent_task_review_dossier::default_profile(),
        manual_finalization: true,
        protected_branches: vec![
            "main".to_string(),
            "master".to_string(),
            "trunk".to_string(),
        ],
    }
}
