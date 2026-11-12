//! Setup for Cook publication behavior tests; assertions stay at the service boundary.

use super::*;

pub(super) fn normal_cook_promotion_fixture(cancel_gate: bool) {
    use crate::agent_task::{AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskAggregateTotals};
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let repository = tempfile::tempdir().expect("repository");
        let path = repository.path();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {:?}: {:?}", args, output);
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        let origin = path.join(".git").display().to_string();
        git(&["remote", "add", "origin", &origin]);
        std::os::unix::fs::symlink("/srv/admitted-assets", path.join("asset-link")).unwrap();
        std::fs::write(path.join("tracked.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "base"]);
        let admitted_head = git_output(path, &["rev-parse", "HEAD"]).unwrap();
        let policy = path
            .join(".git")
            .join(homeboy_core::repository_integrity::OPERATOR_POLICY_FILE);
        std::fs::write(
            &policy,
            format!(
                r#"{{"schema":"homeboy/repository-integrity-policy/v1","origin":"{origin}","symlink_exceptions":[{{"path":"asset-link","target_base64":"L3Nydi9hZG1pdHRlZC1hc3NldHM=","reason":"fixture asset mount"}}]}}"#
            ),
        )
        .unwrap();
        std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o600)).unwrap();
        let snapshot = homeboy_core::source_snapshot::collect_local_checked(
            "local",
            path,
            Some(path.to_str().unwrap()),
            "git",
        )
        .expect("admit source snapshot");
        let evidence = snapshot
            .repository_integrity_evidence
            .clone()
            .expect("evidence");

        std::fs::write(path.join("tracked.txt"), "candidate\n").unwrap();
        let patch = path.join("candidate.patch");
        let output = Command::new("git")
            .args(["diff", "--binary"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        let patch_contents = String::from_utf8(output.stdout).unwrap();
        std::fs::write(&patch, &patch_contents).unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-m", "harvested candidate"]);
        let candidate = crate::agent_task::AgentTaskCandidate {
            base_sha: admitted_head.clone(),
            candidate_sha: git_output(path, &["rev-parse", "HEAD"]).unwrap(),
            tree_sha: git_output(path, &["rev-parse", "HEAD^{tree}"]).unwrap(),
        };
        git(&["reset", "--hard", &admitted_head]);
        std::fs::remove_file(&patch).unwrap();
        std::fs::write(
            &policy,
            format!(
                r#"{{"schema":"homeboy/repository-integrity-policy/v1","origin":"{origin}","symlink_exceptions":[]}}"#
            ),
        )
        .unwrap();

        let run_id = "admitted-policy-normal-promotion";
        let mut options = batch_cook_options(
            "admitted-policy-normal-promotion",
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: Arc::new(AtomicUsize::new(0)),
            }),
        );
        options.identity.initial_run_id = run_id.to_string();
        options.workspace.to_worktree = path.display().to_string();
        options.workspace.source_worktree_path = Some(path.to_path_buf());
        agent_task_lifecycle::submit_plan(&options.identity.initial_plan, Some(run_id)).unwrap();
        let store = test_lifecycle_store();
        agent_task_lifecycle::record_run_aggregate(
            run_id,
            &options.identity.initial_plan,
            &AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: options.identity.initial_plan.plan_id.clone(),
                totals: AgentTaskAggregateTotals::default(),
                outcomes: vec![AgentTaskOutcome {
                    task_id: "provider".to_string(),
                    status: AgentTaskOutcomeStatus::CandidateRecoverable,
                    candidate: Some(candidate),
                    artifacts: vec![AgentTaskArtifact {
                        id: "candidate".to_string(),
                        kind: "patch".to_string(),
                        path: Some(patch.display().to_string()),
                        size_bytes: Some(patch_contents.len() as u64),
                        sha256: Some(homeboy_engine_primitives::content_hash::sha256_hex(
                            patch_contents.as_bytes(),
                        )),
                        metadata: serde_json::json!({
                            "task_id": "provider",
                            "producer_attempt": 1,
                            "run_id": run_id,
                            "base_ref": "main",
                            "provider_backend": "fixture",
                            "repository_identity": "fixture-repository",
                            "workspace_identity": "fixture-workspace",
                        }),
                        ..Default::default()
                    }],
                    outputs: test_review_form_outputs(),
                    metadata: serde_json::json!({ "model": "fixture-model" }),
                    ..Default::default()
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .unwrap();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["source_checkout"] = serde_json::to_value(&snapshot).unwrap();
        })
        .unwrap();

        if cancel_gate {
            options.identity.cook_id = "cook-cancel-live-promotion".to_string();
            let cook_id = options.identity.cook_id.clone();
            store
                .write_cook_index_attempt(
                    &cook_id,
                    1,
                    run_id,
                    "2026-01-01T00:00:00Z".to_string(),
                    None,
                )
                .unwrap();
            let started = repository.path().join("gate-pid");
            options.gates.verify = vec![format!("echo $$ > {}; exec sleep 60", started.display())];
            std::thread::scope(|scope| {
                let cancellation = scope.spawn(|| {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                    while !started.exists() {
                        assert!(std::time::Instant::now() < deadline, "gate did not start");
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    let pid = std::fs::read_to_string(&started).unwrap();
                    agent_task_lifecycle::cancel_run_in_store(
                        &store,
                        &cook_id,
                        Some("stop verification"),
                    )
                    .expect("fence Cook");
                    pid.trim().to_string()
                });
                let promotion = promote_attempt_in_store(&store, &options, run_id)
                    .expect("cancelled promotion report");
                let pid = cancellation.join().unwrap();
                assert!(
                    promotion.deterministic_gates.iter().any(|gate| {
                        gate.termination
                            == crate::agent_task_gate::AgentTaskGateTermination::Cancelled
                    }),
                    "{promotion:?}"
                );
                assert!(
                    !Command::new("kill")
                        .args(["-0", &pid])
                        .stderr(std::process::Stdio::null())
                        .status()
                        .unwrap()
                        .success(),
                    "cancelled gate process is still alive"
                );
            });
            return;
        }

        let promotion =
            promote_attempt_in_store(&store, &options, run_id).expect("normal promotion");
        assert_eq!(
            promotion.repository_integrity_evidence.as_ref(),
            Some(&evidence)
        );

        store
            .record_promotion(run_id, serde_json::to_value(&promotion).unwrap())
            .unwrap();
        let replay = promote_or_load_attempt_in_store(&store, &options, run_id).expect("replay");
        assert_eq!(
            replay.repository_integrity_evidence.as_ref(),
            Some(&evidence)
        );

        std::os::unix::fs::symlink("/srv/unadmitted-assets", path.join("extra-link")).unwrap();
        git(&["add", "extra-link"]);
        git(&["commit", "-m", "unadmitted link"]);
        assert!(
            homeboy_core::repository_integrity::verify_tracked_symlink_portability(
                path,
                "HEAD",
                promotion.repository_integrity_evidence.as_ref(),
            )
            .is_err()
        );
        git(&["reset", "--hard", &admitted_head]);
        git(&[
            "remote",
            "set-url",
            "origin",
            "ssh://wrong.test/homeboy.git",
        ]);
        assert!(
            homeboy_core::repository_integrity::verify_tracked_symlink_portability(
                path,
                "HEAD",
                promotion.repository_integrity_evidence.as_ref(),
            )
            .is_err()
        );
    });
}

pub(super) fn exercise_real_git_base_advance(conflicting: bool, rewritten: bool) {
    use crate::agent_task::{AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskAggregateTotals};

    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temporary repositories");
        let remote = temp.path().join("origin.git");
        let seed = temp.path().join("seed");
        let destination = temp.path().join("destination");
        let advance = temp.path().join("advance");
        let git = |path: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };

        std::fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--bare", "--initial-branch=main"]);
        std::fs::create_dir(&seed).unwrap();
        git(&seed, &["init", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.com"]);
        std::fs::create_dir(seed.join("src")).unwrap();
        std::fs::write(seed.join("src/lib.rs"), "old\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-m", "base"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "main"]);

        std::fs::create_dir(&destination).unwrap();
        git(
            temp.path(),
            &[
                "clone",
                remote.to_str().unwrap(),
                destination.to_str().unwrap(),
            ],
        );
        git(&destination, &["config", "user.name", "Test"]);
        git(&destination, &["config", "user.email", "test@example.com"]);
        std::fs::write(destination.join("src/lib.rs"), "new\n").unwrap();
        let patch = temp.path().join("candidate.patch");
        let patch_output = Command::new("git")
            .args(["diff", "--binary"])
            .current_dir(&destination)
            .output()
            .unwrap();
        assert!(patch_output.status.success());
        std::fs::write(&patch, patch_output.stdout).unwrap();
        let candidate =
            crate::agent_task_promotion::candidate_fingerprint(destination.to_str().unwrap())
                .unwrap();
        let verified_base = git_output(&destination, &["rev-parse", "HEAD"]).unwrap();
        git(&destination, &["add", "."]);
        let candidate_tree = git_output(&destination, &["write-tree"]).unwrap();
        let candidate_sha = git_output(
            &destination,
            &[
                "commit-tree",
                &candidate_tree,
                "-p",
                &verified_base,
                "-m",
                "candidate",
            ],
        )
        .unwrap();

        std::fs::create_dir(&advance).unwrap();
        git(
            temp.path(),
            &["clone", remote.to_str().unwrap(), advance.to_str().unwrap()],
        );
        git(&advance, &["config", "user.name", "Test"]);
        git(&advance, &["config", "user.email", "test@example.com"]);
        for revision in 1..=4 {
            std::fs::write(
                advance.join(format!("base-advanced-{revision}.txt")),
                format!("advanced {revision}\n"),
            )
            .unwrap();
            git(&advance, &["add", "."]);
            git(
                &advance,
                &["commit", "-m", &format!("advance base {revision}")],
            );
        }
        if conflicting {
            std::fs::write(advance.join("src/lib.rs"), "conflicting upstream edit\n").unwrap();
            git(&advance, &["add", "."]);
            git(&advance, &["commit", "-m", "conflicting advance"]);
        }
        if rewritten {
            // Even rewritten ancestry must publish on the candidate's gated base.
            git(&advance, &["checkout", "--orphan", "rewritten-main"]);
            git(&advance, &["add", "."]);
            git(&advance, &["commit", "-m", "rewritten base"]);
            git(&advance, &["push", "--force", "origin", "HEAD:main"]);
        } else {
            git(&advance, &["push", "origin", "main"]);
        }

        let run_id = "moving-base-real-git";
        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut options = batch_cook_options(
            "moving-base-real-git",
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: Arc::clone(&dispatches),
            }),
        );
        options.identity.initial_run_id = run_id.to_string();
        options.finalization.no_finalize = false;
        options.gates = VerifyGateOptions {
            verify: vec![
                "test -f base-advanced-4.txt && test \"$(cat src/lib.rs)\" = new".to_string(),
            ],
            ..Default::default()
        };
        options.workspace.to_worktree = destination.display().to_string();
        options.workspace.source_worktree_path = Some(destination.clone());
        persist_initial_recipe(&options).expect("persist real moving-base recipe");
        agent_task_lifecycle::submit_plan(&options.identity.initial_plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            &options.identity.cook_id,
            1,
            run_id,
        )
        .expect("index real moving-base attempt");
        agent_task_lifecycle::record_run_aggregate(
            run_id,
            &options.identity.initial_plan,
            &AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: options.identity.initial_plan.plan_id.clone(),
                totals: AgentTaskAggregateTotals {
                    succeeded: 1,
                    ..Default::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "provider".to_string(),
                    status: AgentTaskOutcomeStatus::Succeeded,
                    summary: None,
                    failure_classification: None,
                    artifacts: vec![AgentTaskArtifact {
                        id: "candidate".to_string(),
                        kind: "patch".to_string(),
                        path: Some(patch.display().to_string()),
                        ..Default::default()
                    }],
                    typed_artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: test_review_form_outputs(),
                    workflow: None,
                    follow_up: None,
                    metadata: serde_json::json!({ "model": "fixture-model" }),
                    candidate: None,
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .unwrap();
        let applied: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
                "schema": "homeboy/agent-task-promotion-report/v1",
                "status": "applied",
                "source": {"kind": "aggregate", "task_id": "provider", "run_id": run_id},
                "to_worktree": options.workspace.to_worktree,
                "target": {"worktree": options.workspace.to_worktree, "path": destination},
                "candidate": {"base_sha": verified_base, "candidate_sha": candidate_sha, "tree_sha": candidate_tree},
                "changed_files": ["src/lib.rs"],
                "deterministic_gates": [{"id": "gate", "visibility": "visible", "reveal_policy": "full_evidence", "status": "succeeded", "command": ["sh", "-c", "true"], "exit_code": 0}],
                "gate_results": [{"id": "gate", "name": "true", "kind": "command", "status": "passed"}],
                "verified_base": {"base": "main", "sha": verified_base},
                "provenance": {"worktree_path": destination, "candidate": candidate},
                "operator_notification": {"status": "completed", "message": "green"}
            }))
            .unwrap();
        let mut applied = applied;
        applied.candidate_ref = Some(candidate_sha.clone());
        let checkout = serde_json::json!({
            "schema": "homeboy/agent-task-gate-candidate-checkout/v2",
            "commit": candidate_sha,
            "tree": candidate_tree,
        });
        applied.deterministic_gates[0].candidate_checkout =
            Some(serde_json::from_value(checkout).unwrap());
        applied.provenance["candidate_checkout"] = serde_json::to_value(
            applied.deterministic_gates[0]
                .candidate_checkout
                .as_ref()
                .unwrap(),
        )
        .unwrap();
        agent_task_lifecycle::record_promotion(run_id, serde_json::to_value(&applied).unwrap())
            .unwrap();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["provider_executions_consumed"] = serde_json::json!(1);
        })
        .unwrap();
        let terminal_continuation = agent_task_lifecycle::reconcile_status(run_id)
            .unwrap()
            .metadata
            .get("cook_continuation")
            .cloned();
        assert_eq!(
            terminal_continuation.as_ref().map(|value| &value["state"]),
            Some(&serde_json::json!("pending")),
            "the terminal attempt queues its own continuation"
        );
        let finalized = Arc::new(std::sync::Mutex::new(Vec::new()));
        let finalized_seen = Arc::clone(&finalized);
        let report = run_cook(CookContext {
            side_effects: Some(CookSideEffects::new(move |_, _, _, promotion| {
                finalized_seen.lock().unwrap().push(promotion.clone());
                Ok(serde_json::json!({"status": "review_ready"}))
            })),
            ..CookContext::new(options.clone(), Arc::new(UnusedExecutor))
        })
        .unwrap();
        let case = format!("conflicting={conflicting} rewritten={rewritten}");
        assert_eq!(
            report.value.status, "review_ready",
            "{case}: {:#?}",
            report.value
        );
        assert_eq!(report.exit_code, 0, "{case}");
        assert_eq!(
            dispatches.load(Ordering::SeqCst),
            0,
            "{case}: no remediation attempt"
        );
        let finalized = finalized.lock().unwrap();
        assert_eq!(finalized.len(), 1, "{case}: one publication, no retry loop");
        let published = &finalized[0];
        assert_eq!(
            published.status,
            AgentTaskPromotionStatus::Applied,
            "{case}"
        );
        assert_eq!(
            published.verified_base.as_ref().unwrap().sha,
            verified_base,
            "{case}: the candidate is published on its gated base"
        );
        assert_eq!(
            published.candidate.as_ref().unwrap().candidate_sha,
            candidate_sha,
            "{case}: the gated commit is published, not a rebased one"
        );
        assert!(
            published.provenance.get("base_catch_up").is_none(),
            "{case}"
        );
        assert!(
            delivery_of(run_id).blocker.is_none(),
            "{case}: {:?}",
            delivery_of(run_id).blocker
        );
        assert_eq!(
            git_output(&destination, &["rev-parse", "HEAD"]).unwrap(),
            verified_base,
            "{case}: the destination is not rebased"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("src/lib.rs")).unwrap(),
            "new\n",
            "{case}"
        );
    });
}

pub(super) fn run_intentional_no_change_cook(
    evidence_policy: bool,
    verdict: &str,
    with_candidate: bool,
) -> (AgentTaskRunResult<AgentTaskCookReport>, String, CookRequest) {
    let policy = if evidence_policy {
        "evidence"
    } else {
        "change"
    };
    let cook_id = format!("cook-13966-{policy}-{verdict}");
    let run_id = format!("{cook_id}-attempt-1");
    let mut options = batch_cook_options(&cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.identity.initial_run_id = run_id.clone();
    options.finalization.no_finalize = false;
    options.identity.initial_plan.tasks[0].policy.write = "patch".to_string();
    options.identity.initial_plan.tasks[0].policy.apply = "manual".to_string();
    options.identity.initial_plan.tasks[0].expected_artifacts = vec!["patch".to_string()];
    if evidence_policy {
        let request = &mut options.identity.initial_plan.tasks[0];
        request.policy.write = "none".to_string();
        request.policy.apply = "none".to_string();
        request.expected_artifacts.clear();
        request.artifact_declarations.clear();
    }
    persist_initial_recipe(&options).expect("persist Cook recipe");
    agent_task_lifecycle::submit_plan(&options.identity.initial_plan, Some(&run_id))
        .expect("submit Cook attempt");
    agent_task_lifecycle::record_cook_attempt_in_store(
        &test_lifecycle_store(),
        &cook_id,
        1,
        &run_id,
    )
    .expect("record Cook attempt");
    let mut aggregate = review_form_aggregate(&options.identity.initial_plan);
    aggregate.outcomes[0].outputs["provider_run_result"] = serde_json::json!({
        "status": "succeeded",
        "intentional_no_change": {
            "schema": "homeboy/intentional-no-change/v1",
            "verdict": verdict,
            "inspected_revision": "0123456789abcdef",
            "next_action": "Retain the review evidence.",
            "source_evidence": ["provider://review/transcript"]
        }
    });
    aggregate.outcomes[0].metadata["intentional_no_change_verified"] = Value::Bool(true);
    let _candidate_patch = if with_candidate {
        let patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        let file = tempfile::NamedTempFile::new().expect("candidate patch file");
        std::fs::write(file.path(), patch).expect("write candidate patch");
        aggregate.outcomes[0]
            .artifacts
            .push(crate::agent_task::AgentTaskArtifact {
                id: "patch".to_string(),
                kind: "patch".to_string(),
                path: Some(file.path().display().to_string()),
                size_bytes: Some(patch.len() as u64),
                sha256: Some(homeboy_engine_primitives::content_hash::sha256_hex(
                    patch.as_bytes(),
                )),
                metadata: serde_json::json!({
                    "producer_attempt": 1,
                    "base_ref": "main",
                    "provider_backend": "fixture",
                }),
                ..Default::default()
            });
        Some(file)
    } else {
        None
    };
    aggregate.outcomes[0].evidence_refs = vec![crate::agent_task::AgentTaskEvidenceRef {
        kind: "transcript".to_string(),
        uri: "provider://review/transcript".to_string(),
        label: Some("provider review transcript".to_string()),
    }];
    agent_task_lifecycle::record_run_aggregate(&run_id, &options.identity.initial_plan, &aggregate)
        .expect("persist intentional no-change aggregate");

    let promotions = Arc::new(AtomicUsize::new(0));
    let finalizations = Arc::new(AtomicUsize::new(0));
    let resume_options = options.clone();
    let result = run_cook(CookContext {
        side_effects: Some(no_change_side_effects(
            Arc::clone(&promotions),
            Arc::clone(&finalizations),
        )),
        ..CookContext::new(options, Arc::new(UnusedExecutor))
    })
    .expect("finalize intentional no-change Cook");
    assert_eq!(promotions.load(Ordering::SeqCst), 0);
    assert_eq!(finalizations.load(Ordering::SeqCst), 0);
    assert_eq!(result.value.attempts.len(), 1);
    assert!(result.value.attempts[0].promotion.is_none());
    (result, run_id, resume_options)
}

pub(super) fn resume_cook_behind_foreign_promotion_claim(
    live_owner: bool,
) -> (
    AgentTaskRunResult<AgentTaskCookReport>,
    agent_task_lifecycle::AgentTaskRunRecord,
) {
    let recipe_store = CookRecipeStore::from_current_data_root().expect("recipe store");
    let lifecycle_store = test_lifecycle_store();
    let temp = tempfile::tempdir().expect("candidate artifacts");
    let cook_id = if live_owner {
        "cook-15472-live-promotion-owner"
    } else {
        "cook-15472-dead-promotion-owner"
    };
    let run_id = format!("{cook_id}-attempt-1");
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.identity.initial_run_id = run_id.clone();

    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist recipe");
    lifecycle_store
        .submit_plan_with_runtime_admission(&options.identity.initial_plan, &run_id, |_| {
            Ok(serde_json::json!({}))
        })
        .expect("seed lifecycle record");
    lifecycle_store
        .record_cook_attempt(cook_id, 1, &run_id)
        .expect("record Cook attempt");
    seed_candidate_commit_aggregate_in_store(
        &lifecycle_store,
        &run_id,
        &options.identity.initial_plan,
        &temp.path().join("candidate-repo"),
    );
    lifecycle_store
        .mutate_record(&run_id, |record| {
            crate::agent_task_service::set_cook_delivery_artifact_for_test(
                &mut record.metadata,
                "/promotion",
                serde_json::json!({ "status": "verification_pending" }),
            );
            true
        })
        .expect("seed verification-pending continuation");

    // The original Cook process owns the promotion claim and is running gates.
    let mut owner = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn promotion owner fixture");
    let owner_identity = homeboy_core::process::process_start_identity(owner.id())
        .expect("inspect promotion owner")
        .expect("kernel identity");
    let owner_pid = owner.id();
    lifecycle_store
        .mutate_record(&run_id, |record| {
            record.metadata["cook_operation_claims"] = serde_json::json!([{
                "operation_key": promotion_operation_key(&run_id),
                "state": "running",
                "leased_at": "2026-01-01T00:00:00+00:00",
                // Owner identity, not this deadline, decides liveness.
                "lease_deadline": "2026-01-01T00:30:00+00:00",
                "owner_pid": owner_pid,
                "owner_process_start_identity": owner_identity,
            }]);
            true
        })
        .expect("seed foreign promotion claim");
    if !live_owner {
        owner.kill().expect("stop promotion owner fixture");
        owner.wait().expect("reap promotion owner fixture");
    }

    let in_progress_run_id = run_id.clone();
    let mut side_effects = if live_owner {
        // The real claim path observes the live owner's lease.
        CookSideEffects::for_test(
            |store, options, run_id| promote_with_operation_claim_in_store(store, options, run_id),
            |_, _, _, _| unreachable!("a held promotion claim must not finalize"),
        )
    } else {
        // The owner died between the lease observation and classification.
        CookSideEffects::for_test(
            move |_, _, _| {
                Err(homeboy_core::Error::validation_invalid_argument(
                    "promotion_operation",
                    "operation_in_progress",
                    Some(promotion_operation_key(&in_progress_run_id)),
                    None,
                ))
            },
            |_, _, _, _| unreachable!("a held promotion claim must not finalize"),
        )
    };
    let result = run_cook_spine(
        &recipe_store,
        &lifecycle_store,
        options,
        Arc::new(UnusedExecutor),
        &mut side_effects,
        None,
        CookMode::Resume,
    )
    .expect("Cook reports the held promotion claim");
    if live_owner {
        owner.kill().expect("stop promotion owner fixture");
        owner.wait().expect("reap promotion owner fixture");
    }
    let record = lifecycle_store.read_record(&run_id).expect("read Cook run");
    (result, record)
}

pub(super) fn adopt_green_candidate_missing_review_form(
    temp: &std::path::Path,
    cook_id: &str,
    review_executor: SharedAgentTaskExecutor,
) -> AdoptedMissingFormFixture {
    adopt_green_candidate_missing_review_form_with_title(
        temp,
        cook_id,
        review_executor,
        "fix(cook): batch cook",
    )
}

pub(super) fn adopt_green_candidate_missing_review_form_with_title(
    temp: &std::path::Path,
    cook_id: &str,
    review_executor: SharedAgentTaskExecutor,
    title: &str,
) -> AdoptedMissingFormFixture {
    adopt_green_candidate_missing_review_form_with_scope(
        temp,
        cook_id,
        review_executor,
        title,
        None,
    )
}

/// `remediation_scope` dispatches the candidate-producing attempt as a
/// remediation bound to that scope, as a gate-fix or base-conflict attempt is.
pub(super) fn adopt_green_candidate_missing_review_form_with_scope(
    temp: &std::path::Path,
    cook_id: &str,
    review_executor: SharedAgentTaskExecutor,
    title: &str,
    remediation_scope: Option<&[&str]>,
) -> AdoptedMissingFormFixture {
    let source = temp.join("source");
    let target = temp.join("target");
    std::fs::create_dir(&source).expect("create source repository");
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let git_output = |cwd: &std::path::Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("read git output");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&source, &["init", "-b", "main"]);
    git(&source, &["config", "user.email", "agent@example.test"]);
    git(&source, &["config", "user.name", "Agent"]);
    std::fs::write(source.join("lib.rs"), "base\n").unwrap();
    git(&source, &["add", "lib.rs"]);
    git(&source, &["commit", "-m", "base"]);
    let base = git_output(&source, &["rev-parse", "HEAD"]);
    git(
        &source,
        &["remote", "add", "origin", source.to_str().unwrap()],
    );
    git(&source, &["fetch", "origin", "main"]);
    git(
        &source,
        &[
            "worktree",
            "add",
            "-b",
            "fixture-target",
            target.to_str().unwrap(),
            "HEAD",
        ],
    );
    std::fs::write(target.join("lib.rs"), "candidate\n").unwrap();
    git(&target, &["commit", "-am", "candidate"]);
    let candidate = git_output(&target, &["rev-parse", "HEAD"]);
    let provider = temp.join("promotion-provider.sh");
    homeboy_test_support::write_executable(&provider, format!(
            "#!/bin/sh\ncat >/dev/null\ngit -C {target} reset --hard --quiet {candidate}\nprintf '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{target}\",\"command_evidence\":[]}}'\n",
            target = target.display(),
        ), 0o755)
    .unwrap();

    let run_id = format!("{cook_id}-attempt-1");
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.identity.initial_run_id = run_id.to_string();
    options.finalization.title = title.to_string();
    options.workspace.source_worktree_path = None;
    options.workspace.task_base_sha = Some(base.clone());
    options.provider_transport.provider_invocation =
        Some(homeboy_core::command_invocation::CommandInvocation {
            argv: vec![provider.display().to_string()],

            ..Default::default()
        });
    options.provider_transport.attempt_dispatcher = None;
    options.gates.verify = vec!["test \"$(cat lib.rs)\" = candidate".to_string()];
    options.retry_policy.max_attempts = 2;
    options.finalization.no_finalize = false;
    options.finalization.head = Some("fix/8058".to_string());
    options.identity.initial_plan.tasks[0].executor.backend = "fixture-provider".to_string();
    options.identity.initial_plan.tasks[0].executor.model =
        Some("fixture-model-implementation".to_string());
    let workspace_handle = format!("fixture@{cook_id}");
    homeboy_core::worktree::adopt(homeboy_core::worktree::WorktreeAdoptOptions {
        handle: workspace_handle.clone(),
        path: target.display().to_string(),
        kind: Some("test-fixture".to_string()),
        provenance: None,
    })
    .expect("register fixture destination workspace");
    options.workspace.to_worktree = workspace_handle.clone();
    if let Some(scope) = remediation_scope {
        let inputs = &mut options.identity.initial_plan.tasks[0].inputs;
        if !inputs.is_object() {
            *inputs = serde_json::json!({});
        }
        inputs["cook_loop"] = serde_json::json!({ "remediation_scope": scope });
    }
    persist_initial_recipe(&options).unwrap();
    agent_task_lifecycle::submit_plan(&options.identity.initial_plan, Some(&run_id)).unwrap();
    seed_substantive_candidate_aggregate(
        &run_id,
        &options.identity.initial_plan,
        &temp.join("candidate.patch"),
        "diff --git a/lib.rs b/lib.rs\n--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-base\n+candidate\n",
    );
    agent_task_lifecycle::record_cook_attempt_in_store(
        &test_lifecycle_store(),
        cook_id,
        1,
        &run_id,
    )
    .unwrap();

    let mut gate_proof = serde_json::to_value(promotion("fixture-gate-proof")).unwrap();
    gate_proof["to_worktree"] = serde_json::json!(workspace_handle);
    gate_proof["target"] = serde_json::json!({
        "worktree": workspace_handle,
        "path": target.display().to_string(),
    });
    gate_proof["changed_files"] = serde_json::json!(["lib.rs"]);
    gate_proof["verified_base"] = serde_json::json!({ "base": "main", "sha": base });
    let candidate_checkout = serde_json::json!({
        "schema": "homeboy/agent-task-gate-candidate-checkout/v2",
        "commit": candidate,
        "tree": "fixture-candidate-tree",
    });
    gate_proof["deterministic_gates"][0]["command"] =
        serde_json::json!(["sh", "-c", "test \"$(cat lib.rs)\" = candidate"]);
    gate_proof["deterministic_gates"][0]["candidate_checkout"] = candidate_checkout.clone();
    gate_proof["provenance"]["worktree_path"] = serde_json::json!(target.display().to_string());
    gate_proof["provenance"]["candidate_checkout"] = candidate_checkout;
    let mut backend = CaptureBackend {
        synthetic_gate_proof: Some(serde_json::from_value(gate_proof).unwrap()),
        ..Default::default()
    };
    let result = adopt_cook_candidate_with_dispatcher_and_backend(
        cook_id,
        &candidate,
        AgentTaskCandidateAdoptionOptions {
            ai_model: Some("fixture-model-review".to_string()),
            replace_interrupted: false,
        },
        |_| Ok(None),
        review_executor,
        &mut backend,
    );
    AdoptedMissingFormFixture {
        result,
        backend,
        candidate,
    }
}

pub(super) fn record_tracked_promotion_continuation(
    options: &CookRequest,
    target: &std::path::Path,
) {
    let recipe_store = CookRecipeStore::from_current_data_root().unwrap();
    let lifecycle_store = test_lifecycle_store();
    record_tracked_promotion_continuation_in_stores(
        &recipe_store,
        &lifecycle_store,
        options,
        target,
    );
}

pub(super) fn record_tracked_promotion_continuation_in_stores(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    target: &std::path::Path,
) {
    if !recipe_store.recipe_exists(&options.identity.cook_id) {
        recipe_store.persist_initial_recipe(options).unwrap();
    }
    submit_plan_in_test_store(
        lifecycle_store,
        &options.identity.initial_plan,
        Some(&options.identity.initial_run_id),
    )
    .unwrap();
    lifecycle_store
        .mutate_record(&options.identity.initial_run_id, |record| {
            record.metadata["cook_id"] = serde_json::json!(options.identity.cook_id);
            record.metadata["cook_attempt"] = serde_json::json!(1);
            true
        })
        .unwrap();
    lifecycle_store
        .record_cook_attempt(
            &options.identity.cook_id,
            1,
            &options.identity.initial_run_id,
        )
        .unwrap();
    let mut checkpoint = serde_json::to_value(promotion(&options.identity.initial_run_id)).unwrap();
    checkpoint["status"] = serde_json::json!("gate_failed");
    checkpoint["deterministic_gates"][0]["status"] = serde_json::json!("failed");
    checkpoint["deterministic_gates"][0]["exit_code"] = serde_json::json!(1);
    checkpoint["gate_results"][0]["status"] = serde_json::json!("failed");
    let patch = Command::new("git")
        .args(["diff", "--binary", "--full-index"])
        .current_dir(target)
        .output()
        .expect("capture promoted patch");
    assert!(patch.status.success());
    let patch = String::from_utf8(patch.stdout).expect("patch is UTF-8");
    let patch_path = target
        .parent()
        .expect("candidate parent")
        .join("candidate.patch");
    std::fs::write(&patch_path, &patch).expect("persist patch artifact");
    let candidate =
        crate::agent_task_promotion::candidate_fingerprint(target.to_str().expect("target path"))
            .expect("candidate fingerprint");
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } = &candidate
    else {
        panic!("fixture target is a Git worktree");
    };
    let index = tempfile::NamedTempFile::new().unwrap();
    let index_path = index.path().display().to_string();
    let run_git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .env("GIT_INDEX_FILE", &index_path)
            .current_dir(target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    run_git(&["read-tree", "HEAD"]);
    run_git(&["add", "-A"]);
    let tree_sha = run_git(&["write-tree"]);
    let candidate_sha = run_git(&[
        "commit-tree",
        &tree_sha,
        "-p",
        &fingerprint.head,
        "-m",
        "candidate",
    ]);
    run_git(&[
        "update-ref",
        &format!(
            "refs/homeboy/candidates/{}",
            options.identity.initial_run_id
        ),
        &candidate_sha,
    ]);
    let immutable = serde_json::json!({"base_sha":fingerprint.head,"candidate_sha":candidate_sha,"tree_sha":tree_sha});
    checkpoint["to_worktree"] = serde_json::json!(options.workspace.to_worktree);
    checkpoint["target"] = serde_json::json!({
        "worktree": options.workspace.to_worktree,
        "path": target,
        "branch": "cook-candidate",
        "head": fingerprint.head
    });
    checkpoint["candidate"] = immutable.clone();
    checkpoint["changed_files"] = serde_json::json!(fingerprint.changed_files);
    checkpoint["provenance"] = serde_json::json!({
        "post_apply": true,
        "candidate_commit": immutable,
        "candidate": candidate,
        "worktree_path": target,
        "gate_feedback_baseline": {
            "schema": "homeboy/agent-task-gate-feedback-baseline/v1",
            "current_diff": patch
        }
    });
    lifecycle_store
        .record_promotion(&options.identity.initial_run_id, checkpoint)
        .unwrap();
}
