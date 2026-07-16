#![cfg(test)]

use super::*;
use crate::run_lifecycle_record::{
    ArtifactRetentionLifecycle, ArtifactRetentionStatus, CleanupLifecycle, CleanupState,
    ExternalRuntimeId, FinalizationLifecycle, FinalizationState, ProviderRuntimeLifecycle,
    ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
};
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
use std::process::Command;

#[derive(Default)]
struct MockBackend {
    branch: String,
    changed_files: Vec<String>,
    candidate_state: Option<AgentTaskPrCandidateState>,
    existing_pr: Option<AgentTaskPrRef>,
    create_error: bool,
    push_error: bool,
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
        let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } =
            actual
        else {
            unreachable!("test candidate is Git")
        };
        let mut changed_files = options.changed_files.clone();
        changed_files.sort();
        changed_files.dedup();
        if !changed_files.is_empty() && changed_files != fingerprint.changed_files {
            return Err(Error::validation_invalid_argument(
                "changed-file",
                "caller changed files do not match promoted candidate",
                None,
                None,
            ));
        }
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
        _base: &AgentTaskPrResolvedBase,
        _head: &str,
    ) -> Result<AgentTaskPrCandidateState> {
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

    fn commit_all(&mut self, _path: &str, _message: &str) -> Result<()> {
        self.committed = true;
        self.commit_calls += 1;
        Ok(())
    }

    fn push_branch(&mut self, _path: &str, _head: &str) -> Result<()> {
        if self.push_error {
            return Err(Error::git_command_failed("git push failed"));
        }
        self.pushed = true;
        self.push_calls += 1;
        Ok(())
    }

    fn find_open_pr(
        &mut self,
        _path: &str,
        _base: &str,
        _head: &str,
    ) -> Result<Option<AgentTaskPrRef>> {
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

    let report = finalize_pr_with_backend(finalization_options, &mut backend)
        .expect("clean synced candidate publishes");

    assert_eq!(report.status, "review_ready");
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert!(!backend.committed && !backend.pushed);
    assert!(backend.created);
}

#[test]
fn durable_finalization_accepts_succeeded_generic_executor_outcome_once() {
    let mut backend = MockBackend {
        changed_files: vec!["src/lib.rs".to_string()],
        lifecycle: Some(generic_executor_lifecycle(
            ProviderRuntimeState::Succeeded,
            "openai/gpt-5.6-terra",
        )),
        gate_proof: Some(successful_gate_proof()),
        ..Default::default()
    };
    let mut finalization_options = options();
    finalization_options.manual_finalization = false;

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
        let mut rejected_backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(generic_executor_lifecycle(
                rejected_state,
                "openai/gpt-5.6-terra",
            )),
            gate_proof: Some(successful_gate_proof()),
            ..Default::default()
        };
        let mut rejected_options = options();
        rejected_options.manual_finalization = false;

        assert!(finalize_pr_with_backend(rejected_options, &mut rejected_backend).is_err());
        assert!(!rejected_backend.committed);
    }
}

#[test]
fn durable_finalization_rejects_model_less_terminal_record_without_mutation() {
    crate::test_support::with_isolated_home(|_| {
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
        let before = crate::agent_task_lifecycle::status(&record.run_id)
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

        let error = finalize_pr_with_backend(finalization_options, &mut backend)
            .expect_err("model-less terminal record is rejected");
        let after = crate::agent_task_lifecycle::status(&record.run_id)
            .expect("obsolete record remains readable");

        assert_eq!(
            error.code,
            crate::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(before, after);
        assert!(!backend.committed);
        assert!(!backend.pushed);
        assert!(!backend.created);
    });
}

#[test]
fn durable_finalization_accepts_native_and_generic_evidence_but_omits_skipped_work() {
    crate::test_support::with_isolated_home(|_| {
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
        let record = crate::agent_task_lifecycle::record_completed_run(
            &plan,
            &aggregate,
            Some("cook-3678"),
        )
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

        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(record.lifecycle),
            gate_proof: Some(successful_gate_proof()),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.manual_finalization = false;

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
        crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
            .unwrap();
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
        crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
            .unwrap();
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
        crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
            .unwrap();
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
        crate::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
            .unwrap(),
    );
    assert!(finalize_pr_with_backend(
        real_git_finalization_options(repo.path(), vec!["a".to_string()]),
        &mut mismatch
    )
    .is_err());
}

#[test]
fn production_validator_normalizes_changed_file_order_and_duplicates() {
    crate::test_support::with_isolated_home(|_| {
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
fn production_validator_accepts_only_the_exact_promoted_recovery_commit() {
    crate::test_support::with_isolated_home(|_| {
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
        assert!(error.message.contains("changed files must exactly match"));
        let mut synthetic = options.clone();
        synthetic.changed_files = vec!["candidate".to_string(), "synthetic".to_string()];
        let error =
            validate_real_candidate_fingerprint(&synthetic).expect_err("extra paths rejected");
        assert!(error.message.contains("changed files must exactly match"));

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
            lifecycle: None,
        },
        ai_used_for: "Drafted implementation and tests; Chris reviews and owns the change."
            .to_string(),
        review_dossier: AgentTaskReviewDossier {
            schema: "homeboy/agent-task-review-dossier/v1".to_string(),
            summary: "Finalize a verified candidate.".to_string(),
            what_changed: vec!["Updates the finalization contract.".to_string()],
            how_to_test: vec![
                crate::agent_task_review_dossier::AgentTaskReviewTestStep {
                    command: "cargo test agent_task_finalization".to_string(),
                    expected: "passes".to_string(),
                },
            ],
            compatibility: "No compatibility impact.".to_string(),
            evidence: Vec::new(),
            ai_assistance: crate::agent_task_review_dossier::AgentTaskReviewAiAssistance {
                used: true,
                tool: "OpenCode (GPT-5.5)".to_string(),
                model: "GPT-5.5".to_string(),
                used_for: "Drafted implementation and tests; Chris reviews and owns the change."
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
