//! Agent-task command promotion source resolution and review/loop reporting tests.

use super::support::*;
use crate::agents::agent_task_service::DerivedCookBaselineCapability;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// The tests below drive the store-rooted entry points. Resolving the store
/// once here keeps the ambient lookup in one place and lets the ambient
/// wrappers be deleted (#7505).
fn test_lifecycle_store() -> homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore {
    homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
        .expect("lifecycle store")
}

#[test]
fn promotion_source_resolves_completed_run_id() {
    with_temp_home(|| {
        let run_id = "run-promotion-source";

        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(InspectingExecutor::noop(run_id)),
        )
        .expect("run completed");

        let (raw, path) = review::read_promotion_source(run_id).expect("promotion source resolved");

        assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
        assert_eq!(
            path.as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str()),
            Some("aggregate.json")
        );
    });
}

#[test]
fn promotion_source_reads_bare_json_file_path() {
    let file = tempfile::NamedTempFile::new().expect("source file");
    std::fs::write(
        file.path(),
        r#"{"schema":"homeboy/agent-task-aggregate/v1"}"#,
    )
    .expect("write source");

    let (raw, path) = review::read_promotion_source(&file.path().display().to_string())
        .expect("promotion source file resolved");

    assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
    assert_eq!(path.as_deref(), Some(file.path()));
}

#[test]
fn applied_promotion_resume_requires_explicit_gate_rerun() {
    let applied = json!({ "status": "applied" });
    let failed = json!({ "status": "gate_failed" });

    assert!(!review::promotion_is_resumable(&applied, false));
    assert!(review::promotion_is_resumable(&applied, true));
    assert!(review::promotion_is_resumable(&failed, false));
}

#[test]
fn cli_promotion_resume_policy_matches_the_shared_service() {
    for (previous, rerun_completed_gates) in [
        (json!({ "status": "applied" }), false),
        (json!({ "status": "applied" }), true),
        (json!({ "status": "gate_failed" }), false),
        (json!({ "status": "verification_pending" }), false),
        (json!({ "status": "completed" }), true),
    ] {
        assert_eq!(
            review::promotion_is_resumable(&previous, rerun_completed_gates),
            homeboy::agents::agent_task_service::promotion_is_resumable(
                &previous,
                rerun_completed_gates,
            ),
        );
    }
}

#[test]
fn review_reports_queued_run_without_chat_state() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-review-queued"))
            .expect("submitted");

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-queued".to_string(),
            full: true,
            to_worktree: None,
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-review/v1");
        assert_eq!(value["run_id"], "run-review-queued");
        assert_eq!(value["state"], "queued");
        assert_eq!(value["transport"]["chat_state_required"], false);
        assert!(value["aggregate_review"].is_null());
        assert_eq!(value["logs"]["events"][0]["status"], "queued");
        assert!(value["next_actions"][0]
            .as_str()
            .expect("next action")
            .contains("run-next"));
    });
}

#[test]
fn review_reports_completed_aggregate_and_promotion_hints() {
    with_temp_home(|| {
        run_loaded_plan(
            test_plan(),
            Some("run-review-completed"),
            Arc::new(ApplyArtifactExecutor),
        )
        .expect("run completed");

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-completed".to_string(),
            full: true,
            to_worktree: Some("homeboy@fix-review-flow".to_string()),
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["state"], "succeeded");
        assert_eq!(value["durable_read"]["phase"], "controller_local");
        assert!(value["durable_read"]["unavailable_sources"]
            .as_array()
            .expect("durable source availability")
            .is_empty());
        assert_eq!(value["aggregate_review"]["summary"]["apply_candidates"], 1);
        assert_eq!(value["artifacts"]["artifacts"][0]["id"], "patch-a");
        assert_eq!(value["promotion_candidates"][0]["task_id"], "task-a");
        assert_eq!(value["promotion_candidates"][0]["artifact_id"], "patch-a");
        assert_eq!(value["promotion_candidates"][0]["ready"], true);
        assert_eq!(
            value["promotion_candidates"][0]["command"],
            json!([
                "homeboy",
                "agent-task",
                "promote",
                "run-review-completed",
                "--task-id",
                "task-a",
                "--artifact-id",
                "patch-a",
                "--to-worktree",
                "homeboy@fix-review-flow"
            ])
        );
        assert!(value["next_actions"][0]
            .as_str()
            .expect("next action")
            .contains("promotion_candidates"));
    });
}

#[test]
fn default_review_is_bounded_and_points_to_full_evidence() {
    with_temp_home(|| {
        run_loaded_plan(
            test_plan(),
            Some("run-review-default-bounded"),
            Arc::new(ApplyArtifactExecutor),
        )
        .expect("run completed");

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-default-bounded".to_string(),
            full: false,
            to_worktree: None,
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["view"], "summary");
        assert!(value.get("record").is_none());
        assert!(value.get("logs").is_none());
        assert!(value.get("artifacts").is_none());
        assert_eq!(
            value["full_command"],
            "homeboy agent-task review run-review-default-bounded --full"
        );
        assert_eq!(value["canonical_candidate"]["state"], "patch_available");
        assert_eq!(value["selected_candidate"]["size_bytes"], 42);
        assert!(value["promotion_candidates"][0]["command"].is_null());
        assert_eq!(
            value["promotion_candidates"][0]["destination_required"],
            true
        );
        let destination_guidance = value["next_actions"][0]
            .as_str()
            .expect("destination guidance");
        assert!(destination_guidance.contains(
            "homeboy agent-task review run-review-default-bounded --to-worktree <managed-worktree>"
        ));
        assert!(!destination_guidance.contains("agent-task promote"));
    });
}

#[test]
fn full_review_excludes_unrelated_worktree_cleanup_inventory() {
    with_temp_home(|| {
        let run_id = "run-review-scoped-cleanup";
        run_loaded_plan(test_plan(), Some(run_id), Arc::new(ApplyArtifactExecutor))
            .expect("run completed");
        let unrelated_worktrees = (0..59)
            .map(|index| format!("/workspace/unrelated-worktree-{index}"))
            .collect::<Vec<_>>();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["automatic_artifact_retention"] = json!({
                "status": "completed",
                "worktree_count": unrelated_worktrees.len(),
                "worktrees": unrelated_worktrees,
            });
        })
        .expect("persist unrelated cleanup inventory");

        let (review_value, exit_code) = review::review(ReviewArgs {
            run_id: run_id.to_string(),
            full: true,
            to_worktree: None,
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert!(review_value["record"]["metadata"]
            .get("automatic_artifact_retention")
            .is_none());
        assert_eq!(
            review_value["cleanup_evidence"][0]["kind"],
            "automatic_artifact_retention"
        );
        assert_eq!(
            review_value["cleanup_evidence"][0]["command"],
            format!("homeboy agent-task status {run_id} --full")
        );
        assert_eq!(
            review_value["cleanup_evidence"][0]["export_command"],
            format!("homeboy agent-task status {run_id} --full --output <path>")
        );
        assert!(!review_value.to_string().contains("unrelated-worktree-58"));
        let persisted = agent_task_lifecycle::status(run_id).expect("cleanup evidence persists");
        assert_eq!(
            persisted.metadata["automatic_artifact_retention"]["worktrees"]
                .as_array()
                .map(Vec::len),
            Some(59)
        );
    });
}

#[test]
fn cook_readers_keep_the_substantive_candidate_after_a_no_change_retry() {
    struct PatchExecutor {
        path: String,
        run_id: String,
        size_bytes: u64,
        sha256: String,
    }

    struct NoChangeReviewExecutor;

    impl AgentTaskExecutorAdapter for NoChangeReviewExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("intentional no-change gate retry".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: json!({
                    "review_form": {
                        "summary": "Retry reviewed the candidate.",
                        "what_changed": ["No additional patch was needed."],
                        "compatibility": "No additional compatibility impact.",
                        "used_for": "Reviewed failed-gate evidence."
                    }
                }),
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    impl AgentTaskExecutorAdapter for PatchExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("produced patch before failed gate".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "candidate".to_string(),
                    kind: "patch".to_string(),
                    name: Some("candidate.patch".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(self.path.clone()),
                    url: None,
                    mime: Some("text/x-diff".to_string()),
                    size_bytes: Some(self.size_bytes),
                    sha256: Some(self.sha256.clone()),
                    metadata: Value::Null,
                }],
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "plan".to_string(),
                    uri: format!("homeboy://agent-task/run/{}/plan#task=task-a", self.run_id),
                    label: None,
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    with_temp_home(|| {
        let cook_id = "cook-reader-substantive-candidate";
        let candidate_run_id = "cook-reader-substantive-candidate-attempt-1";
        let retry_run_id = "cook-reader-substantive-candidate-attempt-2";
        let patch = tempfile::NamedTempFile::new().expect("candidate patch");
        let patch_contents = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        std::fs::write(patch.path(), patch_contents).expect("write candidate patch");
        run_loaded_plan(
            test_plan(),
            Some(candidate_run_id),
            Arc::new(PatchExecutor {
                path: patch.path().display().to_string(),
                run_id: candidate_run_id.to_string(),
                size_bytes: patch_contents.len() as u64,
                sha256: homeboy_engine_primitives::content_hash::sha256_hex(
                    patch_contents.as_bytes(),
                ),
            }),
        )
        .expect("substantive candidate completed");
        run_loaded_plan(
            test_plan(),
            Some(retry_run_id),
            Arc::new(NoChangeReviewExecutor),
        )
        .expect("intentional no-change retry completed");
        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            cook_id,
            1,
            candidate_run_id,
        )
        .expect("record substantive candidate");
        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            cook_id,
            2,
            retry_run_id,
        )
        .expect("record latest no-change retry");
        agent_task_lifecycle::record_promotion(
            retry_run_id,
            json!({
                "status": "gate_failed",
                "gate_results": [{ "name": "cargo test", "exit_code": 1 }],
                "provenance": { "gate_retry": "intentional_no_change" }
            }),
        )
        .expect("record retry verification provenance");

        let (status_value, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: false,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("Cook status is bounded");
        let (review_value, _) = review::review(ReviewArgs {
            run_id: cook_id.to_string(),
            full: true,
            to_worktree: None,
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("Cook review is bounded");
        let (diagnose_value, _) = diagnose(DiagnoseArgs {
            run_id: cook_id.to_string(),
            full: false,
        })
        .expect("Cook diagnosis is bounded");
        let (evidence_value, _) = evidence(EvidenceArgs {
            run_id: cook_id.to_string(),
            kind: None,
            task: None,
            failure_only: true,
            full: false,
        })
        .expect("Cook failure evidence is bounded");
        let (all_evidence_value, _) = evidence(EvidenceArgs {
            run_id: cook_id.to_string(),
            kind: None,
            task: None,
            failure_only: false,
            full: false,
        })
        .expect("Cook evidence reads the selected candidate plan");

        for value in [
            &status_value,
            &review_value,
            &diagnose_value,
            &evidence_value,
        ] {
            assert_eq!(value["run_id"], candidate_run_id);
            assert_eq!(
                value["candidate_selection"]["latest_attempt_run_id"],
                retry_run_id
            );
            assert_eq!(value["candidate_selection"]["run_id"], candidate_run_id);
        }
        assert_eq!(review_value["contributing_attempt"]["run_id"], retry_run_id);
        assert_eq!(
            review_value["contributing_attempt"]["review_form"]["summary"],
            "Retry reviewed the candidate."
        );
        assert_eq!(
            review_value["contributing_attempt"]["verification"]["provenance"]["gate_retry"],
            "intentional_no_change"
        );
        assert_eq!(
            all_evidence_value["evidence"][0]["content"]["value"]["task_id"], "task-a",
            "{all_evidence_value:#}"
        );

        let (bridge_value, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            exact: false,
            bridge: true,
            since_cursor: Some(0),
            full: false,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("Cook bridge status selects the candidate");
        assert_eq!(bridge_value["schema"], "homeboy/agent-task-run-status/v1");
        assert_eq!(bridge_value["run_id"], candidate_run_id);
        assert_eq!(
            bridge_value["candidate_selection"]["run_id"],
            candidate_run_id
        );

        let (attempt_status, _) = status(StatusArgs {
            run_id: retry_run_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: false,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("exact attempt remains directly addressable");
        assert_eq!(attempt_status["run_id"], retry_run_id);
    });
}

#[test]
fn detached_cook_parent_status_projects_its_materializing_child_before_index_publication() {
    with_temp_home(|| {
        let cook_id = "cook-detached-status-parent";
        let child_run_id = "cook-detached-status-parent-attempt-1";
        agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
            &test_lifecycle_store(),
            cook_id,
        )
        .expect("record detached Cook parent");
        agent_task_lifecycle::reserve_detached_cook_handoff_materialization_in_store(
            &test_lifecycle_store(),
            cook_id,
            child_run_id,
        )
        .expect("reserve detached Cook child");

        let (reserved_status, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("reserved parent remains readable before child submission");
        assert_eq!(reserved_status["run_id"], cook_id);

        agent_task_lifecycle::submit_plan(&test_plan(), Some(child_run_id))
            .expect("materialize detached Cook child");
        agent_task_lifecycle::rewrite_record_for_test(child_run_id, |record| {
            record.metadata["provider_executions"] = json!([{
                "key": "fixture-task:1",
                "state": "running",
            }]);
        })
        .expect("record provider boundary");
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("resolve lifecycle store")
            .record_cook_progress_with_activity(
                child_run_id,
                "provider_start",
                1,
                Some("fixture provider"),
                None,
            )
            .expect("record provider start");

        let (materializing_status, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("parent projects materializing child");
        assert_eq!(materializing_status["run_id"], child_run_id);
        assert_eq!(
            materializing_status["tasks"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            materializing_status["metadata"]["cook_progress"]["phase"],
            "provider_start"
        );
        assert_eq!(
            materializing_status["liveness"]["provider_boundary"]["status"],
            "recorded"
        );
        assert_eq!(
            materializing_status["identity"]["requested_run_id"],
            cook_id
        );
        assert_eq!(
            materializing_status["identity"]["resolved_run_id"],
            child_run_id
        );
        assert_eq!(
            materializing_status["identity"]["resolution"],
            "detached_materializing_attempt"
        );

        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            cook_id,
            1,
            child_run_id,
        )
        .expect("publish Cook index");
        let (published_status, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("published Cook index supersedes the materialization reservation");
        assert_eq!(published_status["run_id"], child_run_id);
        assert_eq!(published_status["identity"]["resolution"], "default");
        assert_eq!(
            published_status["identity"]["cook_alias"]["latest_attempt_run_id"],
            child_run_id
        );
        let (exact_parent_status, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            exact: true,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("parent remains an immutable exact read after publication");
        assert_eq!(exact_parent_status["run_id"], cook_id);
        assert_eq!(
            exact_parent_status["identity"]["resolution"],
            "exact_record"
        );
        assert_eq!(
            exact_parent_status["identity"]["cook_alias"]["latest_attempt_run_id"],
            child_run_id
        );
    });
}

#[test]
fn exact_status_inspects_initial_cook_record_after_alias_advances() {
    with_temp_home(|| {
        let cook_id = "cook-exact-initial-record";
        let retry_run_id = "cook-exact-initial-record-attempt-2";
        run_loaded_plan(
            test_plan(),
            Some(cook_id),
            Arc::new(InspectingExecutor::noop(cook_id)),
        )
        .expect("initial Cook record completed");
        run_loaded_plan(
            test_plan(),
            Some(retry_run_id),
            Arc::new(InspectingExecutor::noop(retry_run_id)),
        )
        .expect("retry Cook record completed");
        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            cook_id,
            1,
            cook_id,
        )
        .expect("record initial Cook attempt");
        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            cook_id,
            2,
            retry_run_id,
        )
        .expect("record later Cook attempt");

        let (default_status, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("default status resolves Cook alias");
        assert_eq!(default_status["run_id"], retry_run_id);
        assert_eq!(default_status["identity"]["requested_run_id"], cook_id);
        assert_eq!(default_status["identity"]["resolved_run_id"], retry_run_id);
        assert_eq!(
            default_status["identity"]["cook_alias"]["latest_attempt_run_id"],
            retry_run_id
        );

        let (exact_status, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            exact: true,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("exact status reads initial Cook record");
        assert_eq!(exact_status["run_id"], cook_id);
        assert_eq!(exact_status["identity"]["requested_run_id"], cook_id);
        assert_eq!(exact_status["identity"]["resolved_run_id"], cook_id);
        assert_eq!(exact_status["identity"]["resolution"], "exact_record");
        assert_eq!(
            exact_status["identity"]["cook_alias"]["latest_attempt_run_id"],
            retry_run_id
        );
    });
}

#[test]
fn cook_preserves_successful_candidate_when_provider_response_has_wrong_schema() {
    with_temp_home(|| {
        let root = tempfile::tempdir().expect("worktree root");
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir(&source).expect("create source");
        init_runtime_component_checkout(&source);
        let status = Command::new("git")
            .args([
                "-C",
                source.to_str().expect("source path"),
                "remote",
                "add",
                "origin",
                "https://github.com/Extra-Chill/homeboy.git",
            ])
            .status()
            .expect("configure source remote");
        assert!(status.success());
        let status = Command::new("git")
            .args([
                "-C",
                source.to_str().expect("source path"),
                "worktree",
                "add",
                "-b",
                "fixture-wrong-schema",
                target.to_str().expect("target path"),
            ])
            .status()
            .expect("create target worktree");
        assert!(status.success());
        let provider = root.path().join("worktree-provider.sh");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"{}\",\"path\":\"{}\",\"branch\":\"fixture-wrong-schema\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
                target.display(),
                target.display(),
            ),
        )
        .expect("write worktree provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("worktree provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions)
            .expect("make worktree provider executable");
        let mut config = homeboy::core::defaults::load_config();
        // This fixture exercises the provider response boundary, not host
        // capacity admission. Keep that independent inside its isolated home.
        config.retention.reconstructable_artifact_reserve_bytes = 0;
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![provider.display().to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(
                    homeboy::core::defaults::WorktreeProviderListResultMapping {
                        items: "$.worktrees".to_string(),
                        handle: "$.handle".to_string(),
                        path: "$.path".to_string(),
                        branch: "$.branch".to_string(),
                        dirty: "$.safety.dirty".to_string(),
                        unpushed: "$.safety.unpushed".to_string(),
                        primary: "$.safety.primary".to_string(),
                        task_url: None,
                    },
                ),
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save worktree provider config");
        let (value, exit_code) = run_cook_with_executor(
            AgentTaskCookArgs {
                help: None,
                help_full: None,
                provider_evidence_inputs: Vec::new(),
                dispatch: DispatchArgs {
                    prompt: None,
                    prompt_is_literal: false,
                    tasks: Vec::new(),
                    cwd: None,
                    workspace: None,
                    repo: Some("homeboy".to_string()),
                    task_url: Some(
                        "https://github.com/Extra-Chill/homeboy/issues/3675".to_string(),
                    ),
                    backend: Some("fixture".to_string()),
                    selector: None,
                    model: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    concurrency: 1,
                    run_id: Some("cook-missing-provider".to_string()),
                    core: DispatchCoreArgs {
                        tasks_json: None,
                        provider_config: None,
                        client_context: None,
                        // `max_attempts: 2` below needs a budget that can fund two
                        // provider-backed attempts and one same-provider remediation,
                        // or `validate_effective_cook_budget` rejects at preflight and
                        // the wrong-schema behaviour under test is never reached.
                        attempts: Some(2),
                        same_provider_retries: Some(1),
                        provider_rotations: Some(0),
                        queue_only: false,
                        timeout_ms: None,
                        resolved_provider_policy: None,
                        deny_command: Vec::new(),
                        allow_command: Vec::new(),
                        command_policy_reason: None,
                    },
                },
                candidate_completion: homeboy::agents::agent_task_scheduler::AgentTaskCandidateCompletionPolicy::WaitAll,
                attempt_run_id: Some("cook-missing-provider-attempt-1-controller".to_string()),
                attempt_plan: None,
                preview: false,
                goal: Some("cook fixture".to_string()),
                to_worktree: Some(target.display().to_string()),
                resolved_worktree_provider_id: None,
                provider_command: None,
                provider_argv: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf '%s' '{\"schema\":\"homeboy/agent-task-promotion-apply-request/v1\"}'"
                        .to_string(),
                ],
                gates: VerifyGateArgs {
                    accept_inherited_failures: false,
                    gate_package_artifacts: Vec::new(),
                    gate_extension_inputs: Vec::new(),
                    verify: vec!["cargo test --lib".to_string()],
                    verify_file: Vec::new(),
                    private_verify: Vec::new(),
                    private_verify_file: Vec::new(),
                    input_sources: Vec::new(),
                    private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
                    gate_execution_policy: "ordered-fail-fast".to_string(),
                    gate_timeout_seconds: 30 * 60,
                    gate_heartbeat_interval_seconds: 5,
                    gate_no_progress_timeout_seconds: 5 * 60,
                    rerun_completed_gates: false,
                    gate_environment_mode: "inherit".to_string(),
                    gate_environment: Vec::new(),
                    gate_environment_preserve: Vec::new(),
                    gate_toolchains: Vec::new(),
                    gate_toolchain_specs: Vec::new(),
                    isolate_gate_home: true,
                    isolate_gate_xdg: true,
                    gate_shared_cargo_target: false,
                    no_gate_shared_cargo_target: false,
                },
                max_attempts: 2,
                no_finalize: false,
                draft_pr: false,
                full: true,
                no_progress: false,
                base: Some("main".to_string()),
                head: None,
                title: None,
                commit_message: None,
                protected_branches: review::default_protected_branches(),
                ai_tool: "OpenCode (GPT-5.5)".to_string(),
                ai_used_for: "test".to_string(),
                require_acceptance: false,
                acceptance_authority: None,
                acceptance_policy: None,
                repository_identity: None,
                base_resolution: None,
                prompt_snapshot: None,
            },
            Arc::new(ExtensionProviderAgentTaskExecutor::default()),
        )
        .expect("cook reported controlled failure");

        assert_eq!(exit_code, 1);
        assert_eq!(value["schema"], "homeboy/agent-task-cook/v1");
        assert_eq!(value["cook_id"], "cook-missing-provider");
        assert_eq!(
            value["latest_run_id"],
            "cook-missing-provider-attempt-1-controller"
        );
        assert_eq!(
            value["history_run_ids"].as_array().map(Vec::len),
            Some(1),
            "{value:#}"
        );
        assert_eq!(value["status"], "durable_failure", "{value:#}");
        assert_eq!(value["attempts"][0]["run_id"], value["latest_run_id"]);
        assert!(!value["stop_reason"]
            .as_str()
            .expect("stop reason")
            .is_empty());
        assert_eq!(
            value["failure_context"]["diagnostic"]["details"]["problem"],
            "expected homeboy/agent-task-promotion-apply-response/v1, got homeboy/agent-task-promotion-apply-request/v1"
        );
        assert_eq!(
            value["failure_context"]["next_actions"][0]["command"],
            "homeboy agent-task status cook-missing-provider-attempt-1-controller --full"
        );
        let lifecycle = lifecycle_status("cook-missing-provider-attempt-1-controller")
            .expect("successful candidate remains in durable lifecycle");
        assert_eq!(lifecycle.state, AgentTaskRunState::Succeeded);
    });
}

#[derive(Debug, Clone)]
struct CommittingExecutor {
    workspace: std::path::PathBuf,
}

impl AgentTaskExecutorAdapter for CommittingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let workspace = std::path::PathBuf::from(
            request
                .workspace
                .root
                .as_deref()
                .expect("isolated workspace"),
        );
        assert_ne!(
            workspace, self.workspace,
            "executor must not receive the source workspace"
        );
        std::fs::write(workspace.join("agent-change.txt"), "committed work\n")
            .expect("write executor change");
        let status = Command::new("git")
            .args(["add", "agent-change.txt"])
            .current_dir(&workspace)
            .status()
            .expect("stage executor change");
        assert!(status.success());
        let status = Command::new("git")
            .args(["commit", "-m", "agent: make committed change"])
            .current_dir(&workspace)
            .status()
            .expect("commit executor change");
        assert!(status.success());

        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("committed work".to_string()),
            failure_classification: None,
            artifacts: vec![
                AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "agent-result".to_string(),
                    kind: "agent_result".to_string(),
                    name: Some("agent-result.json".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(workspace.join("plugin.php").display().to_string()),
                    url: None,
                    mime: Some("application/json".to_string()),
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                },
                AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "transcript".to_string(),
                    kind: "transcript".to_string(),
                    name: Some("transcript.log".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(workspace.join("plugin.php").display().to_string()),
                    url: None,
                    mime: Some("text/plain".to_string()),
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                },
            ],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

/// Mimics the typed Lab lifecycle mirror: the provider executes elsewhere, but
/// the completed aggregate is written under the controller-owned attempt id.
#[derive(Debug, Clone)]
struct MirroredAttemptDispatcher {
    executor: Arc<CommittingExecutor>,
    prepared: Arc<std::sync::atomic::AtomicBool>,
}

impl crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher
    for MirroredAttemptDispatcher
{
    fn durable_recipe(&self) -> homeboy::core::Result<serde_json::Value> {
        Ok(serde_json::json!({ "kind": "local" }))
    }

    fn prepare_for_cook(&self) -> homeboy::core::Result<()> {
        self.prepared
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> homeboy::core::Result<()> {
        assert!(
            self.prepared.load(std::sync::atomic::Ordering::SeqCst),
            "cook must prepare the dispatcher before pinning and dispatching its attempt"
        );
        homeboy::agents::agent_tasks::service::run_loaded_plan(
            plan,
            Some(run_id),
            self.executor.clone(),
        )
        .map(|_| ())
    }
}

#[test]
fn cook_promotes_mirrored_remote_attempt_into_controller_target() {
    with_temp_home(|| {
        let mut config = homeboy::core::defaults::load_config();
        config.agent_task.rotation = Some(
            serde_json::to_value(
                homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationPolicy {
                    entries: vec![
                        homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationEntry {
                            model: Some("openai/gpt-5.6-terra".to_string()),
                            ..Default::default()
                        },
                        homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationEntry {
                            model: Some("fallback-model".to_string()),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            )
            .expect("serialize provider rotation policy"),
        );
        homeboy::core::defaults::save_config(&config).expect("save provider rotation");
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        std::fs::create_dir(&source).expect("create source");
        init_runtime_component_checkout(&source);
        let status = Command::new("git")
            .args([
                "-C",
                source.to_str().expect("source path"),
                "remote",
                "add",
                "origin",
                source.to_str().expect("source path"),
            ])
            .status()
            .expect("configure source remote");
        assert!(status.success());
        let status = Command::new("git")
            .args([
                "-C",
                source.to_str().expect("source path"),
                "fetch",
                "origin",
            ])
            .status()
            .expect("fetch source base");
        assert!(status.success());
        let status = Command::new("git")
            .args([
                "-C",
                source.to_str().expect("source path"),
                "worktree",
                "add",
                "-b",
                "fixture-promoted",
                target.to_str().expect("target path"),
                "main",
            ])
            .status()
            .expect("create declared target worktree");
        assert!(status.success());
        let provider = temp.path().join("worktree-provider.sh");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nset -eu\nif [ \"$1\" = resolve ]; then\n  if [ -f '{}/.git' ]; then\n    printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@promoted\",\"path\":\"{}\",\"branch\":\"fixture-promoted\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n  else\n    exit 1\n  fi\nelse\n  git -C '{}' worktree add -b \"$5\" '{}' \"$4\" >/dev/null\nfi\n",
                target.display(),
                target.display(),
                source.display(),
                target.display(),
            ),
        )
        .expect("write worktree provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("worktree provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions)
            .expect("make worktree provider executable");
        let mut config = homeboy::core::defaults::load_config();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![
                        provider.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
                    ]),
                    resolve_not_found_exit_codes: vec![1],
                    ensure: Some(vec![
                        provider.display().to_string(),
                        "ensure".to_string(),
                        "{handle}".to_string(),
                        "{repo}".to_string(),
                        "{base}".to_string(),
                        "{head}".to_string(),
                        "{task_url}".to_string(),
                        "{idempotency_key}".to_string(),
                    ]),
                    ..Default::default()
                },
                list_result_mapping: Some(
                    homeboy::core::defaults::WorktreeProviderListResultMapping {
                        items: "$.worktrees".to_string(),
                        handle: "$.handle".to_string(),
                        path: "$.path".to_string(),
                        branch: "$.branch".to_string(),
                        dirty: "$.safety.dirty".to_string(),
                        unpushed: "$.safety.unpushed".to_string(),
                        primary: "$.safety.primary".to_string(),
                        task_url: None,
                    },
                ),
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save worktree provider config");
        std::fs::write(source.join("pre-existing-candidate.txt"), "preserve me\n")
            .expect("write pre-existing candidate");
        let expected_patch = temp.path().join("expected.patch");
        let promotion_request = temp.path().join("promotion-request.json");
        std::fs::write(
            &expected_patch,
            "diff --git a/agent-change.txt b/agent-change.txt\nnew file mode 100644\nindex 0000000..f3f8b32\n--- /dev/null\n+++ b/agent-change.txt\n@@ -0,0 +1 @@\n+committed work\n",
        )
        .expect("write expected patch");
        let provider = temp.path().join("promotion-provider.sh");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nset -eu\ncat > {}\ngit -C {} apply {}\nprintf '%s\\n' '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{}\"}}'\n",
                promotion_request.display(),
                target.display(),
                expected_patch.display(),
                target.display(),
            ),
        )
        .expect("write promotion provider");

        let executor = Arc::new(CommittingExecutor {
            workspace: target.clone(),
        });
        let prepared = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (value, exit_code) = run_cook_with_executor_and_dispatcher(
            AgentTaskCookArgs {
                help: None,
                help_full: None,
                provider_evidence_inputs: Vec::new(),
                dispatch: DispatchArgs {
                    prompt: Some("commit a change".to_string()),
                    prompt_is_literal: false,
                    tasks: Vec::new(),
                    cwd: Some(target.display().to_string()),
                    workspace: None,
                    repo: Some("fixture-component".to_string()),
                    task_url: Some(
                        "https://github.com/Extra-Chill/homeboy/issues/9908".to_string(),
                    ),
                    backend: Some("fixture".to_string()),
                    selector: None,
                    model: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    concurrency: 1,
                    run_id: Some("cook-committed-work".to_string()),
                    core: DispatchCoreArgs {
                        tasks_json: None,
                        provider_config: None,
                        client_context: None,
                        attempts: Some(1),
                        same_provider_retries: Some(0),
                        provider_rotations: Some(0),
                        queue_only: false,
                        timeout_ms: None,
                        resolved_provider_policy: None,
                        deny_command: Vec::new(),
                        allow_command: Vec::new(),
                        command_policy_reason: None,
                    },
                },
                candidate_completion: homeboy::agents::agent_task_scheduler::AgentTaskCandidateCompletionPolicy::WaitAll,
                attempt_run_id: None,
                attempt_plan: None,
                preview: false,
                goal: None,
                to_worktree: Some(target.display().to_string()),
                resolved_worktree_provider_id: None,
                provider_command: None,
                provider_argv: vec!["sh".to_string(), provider.display().to_string()],
                gates: VerifyGateArgs {
                    accept_inherited_failures: false,
                    gate_package_artifacts: Vec::new(),
                    gate_extension_inputs: Vec::new(),
                    verify: vec!["true".to_string()],
                    verify_file: Vec::new(),
                    private_verify: Vec::new(),
                    private_verify_file: Vec::new(),
                    input_sources: Vec::new(),
                    private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                    gate_execution_policy: "ordered-fail-fast".to_string(),
                    gate_timeout_seconds: 30 * 60,
                    gate_heartbeat_interval_seconds: 5,
                    gate_no_progress_timeout_seconds: 5 * 60,
                    rerun_completed_gates: false,
                    gate_environment_mode: "inherit".to_string(),
                    gate_environment: Vec::new(),
                    gate_environment_preserve: Vec::new(),
                    gate_toolchains: Vec::new(),
                    gate_toolchain_specs: Vec::new(),
                    isolate_gate_home: true,
                    isolate_gate_xdg: true,
                    gate_shared_cargo_target: false,
                    no_gate_shared_cargo_target: false,
                },
                max_attempts: 1,
                no_finalize: true,
                draft_pr: false,
                full: true,
                no_progress: false,
                base: Some("main".to_string()),
                head: Some("fixture-promoted".to_string()),
                title: None,
                commit_message: None,
                protected_branches: review::default_protected_branches(),
                ai_tool: "OpenCode (GPT-5.6 Sol)".to_string(),
                ai_used_for: "test".to_string(),
                require_acceptance: false,
                acceptance_authority: None,
                acceptance_policy: None,
                repository_identity: None,
                base_resolution: None,
                prompt_snapshot: None,
            },
            executor.clone(),
            Some(Arc::new(MirroredAttemptDispatcher {
                executor,
                prepared: prepared.clone(),
            })),
        )
        .expect("cook completes");

        assert!(prepared.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(exit_code, 0, "{value:#}");
        assert_eq!(value["status"], "green_no_finalize");
        assert_eq!(
            value["attempts"][0]["feedback"]["status"],
            "green_completed"
        );
        assert!(value["finalization"].is_null());
        let attempt_run_id = value["attempts"][0]["run_id"]
            .as_str()
            .expect("cook report attempt run id");
        let lifecycle = lifecycle_status(attempt_run_id).expect("local cook lifecycle");
        assert_eq!(
            lifecycle.metadata["worktree_provision"]["action"],
            "existing"
        );
        assert_eq!(lifecycle.lifecycle.provider_runtime.len(), 1);
        assert_eq!(
            lifecycle.lifecycle.provider_runtime[0].metadata["model"],
            "openai/gpt-5.6-terra"
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["patch_artifact"]["id"],
            "cook-fixture-component-attempt-1-committed-changes"
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["changed_files"],
            json!(["agent-change.txt"])
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["provenance"]["artifact_metadata"]["change_source"],
            "local_commits"
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["provenance"]["artifact_metadata"]
                ["artifact_provenance"],
            "homeboy_generated_committed_patch"
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["provenance"]["artifact_metadata"]["commits"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            std::fs::read_to_string(target.join("agent-change.txt")).expect("target patch applied"),
            "committed work\n"
        );
        let request: Value = serde_json::from_str(
            &std::fs::read_to_string(&promotion_request).expect("read promotion request"),
        )
        .expect("typed promotion request");
        assert_eq!(
            request["schema"],
            "homeboy/agent-task-promotion-apply-request/v1"
        );
        assert_eq!(request["to_workspace"], target.display().to_string());
        assert_eq!(request["changed_files"], json!(["agent-change.txt"]));
        assert!(request["patch"]
            .as_str()
            .expect("inline selected patch")
            .contains("committed work"));
        assert!(
            !request["patch"]
                .as_str()
                .expect("inline selected patch")
                .contains("pre-existing-candidate.txt"),
            "promotion receives only the provider delta"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("pre-existing-candidate.txt"))
                .expect("pre-existing candidate preserved"),
            "preserve me\n"
        );
    });
}
