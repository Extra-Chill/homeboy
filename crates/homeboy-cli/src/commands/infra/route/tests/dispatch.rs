#![cfg(test)]

use super::*;

#[test]
fn automatic_local_trace_does_not_target_an_agent_task_lifecycle_record() {
    assert!(placement_outcome_target(None, None).is_none());
}

#[test]
fn successful_offloaded_trace_does_not_target_an_agent_task_lifecycle_record() {
    assert!(placement_outcome_target(None, None).is_none());
}

#[test]
fn detached_fanout_observation_does_not_target_an_agent_task_lifecycle_record() {
    assert!(placement_outcome_target(None, None).is_none());
}

#[test]
fn durable_agent_task_handoff_targets_its_lifecycle_record() {
    assert_eq!(
        placement_outcome_target(Some("durable-run"), None),
        Some(ExecutionPlacementOutcomeTarget::AgentTaskLifecycle {
            run_id: "durable-run"
        })
    );
}

#[test]
fn detached_planless_handoff_persists_explicit_bench_label_before_handoff() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source workspace");
        let decision = fixture_preflight_decision(
            &Cli::parse_from(["homeboy", "status"]),
            Some("homeboy-lab"),
            "bench",
            Some(source.path()),
        )
        .expect("placement decision");
        let handoff = materialize_generic_detached_lab_handoff(
            &[
                "homeboy".to_string(),
                "bench".to_string(),
                "--run-id".to_string(),
                "ssi-fixture-37-20260727-runtime-fixed".to_string(),
            ],
            source.path(),
            &lab_offload_command(&Cli::parse_from(["homeboy", "bench"]).command)
                .expect("bench contract")
                .expect("portable bench"),
            decision.clone(),
        )
        .expect("persist detached bench handoff");

        assert_eq!(handoff.run_id, "ssi-fixture-37-20260727-runtime-fixed");
        let record = agent_task_lifecycle::reconcile_status(&handoff.run_id)
            .expect("interrupted caller leaves a discoverable run");
        assert!(!record.state.is_terminal());
        assert_eq!(record.plan_id, handoff.plan.plan_id);
        assert_eq!(
            agent_task_lifecycle::load_controller_plan(&handoff.run_id)
                .expect("durable controller plan")
                .metadata["execution_placement_decision"]["decision_id"],
            decision.decision_id,
        );
    });
}

#[test]
fn detached_planless_handoff_reuses_the_same_explicit_bench_label() {
    crate::test_support::with_isolated_home(|_| {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--run-id=ssi-fixture-37-20260727-runtime-fixed".to_string(),
        ];
        let source = tempfile::tempdir().expect("source workspace");
        let decision = fixture_preflight_decision(
            &Cli::parse_from(["homeboy", "status"]),
            Some("homeboy-lab"),
            "bench",
            Some(source.path()),
        )
        .expect("placement decision");
        let command = lab_offload_command(&Cli::parse_from(["homeboy", "bench"]).command)
            .expect("bench contract")
            .expect("portable bench");
        let first = materialize_generic_detached_lab_handoff(
            &args,
            source.path(),
            &command,
            decision.clone(),
        )
        .expect("first handoff");
        let second =
            materialize_generic_detached_lab_handoff(&args, source.path(), &command, decision)
                .expect("replayed handoff");

        assert_eq!(first.run_id, second.run_id);
        assert_eq!(first.plan.plan_id, second.plan.plan_id);
        assert!(agent_task_lifecycle::reconcile_status(&first.run_id).is_ok());
    });
}

#[test]
fn failed_detached_bench_retry_replays_the_persisted_workspace_and_inputs() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source workspace");
        git_init(source.path());
        let original = [
            "homeboy",
            "--placement",
            "lab",
            "--detach-after-handoff",
            "bench",
            "blocks-engine",
            "--run-id",
            "bench-pre-provider-failure",
            "--rig",
            "ssi-fixtures",
            "--extension",
            "blocks-engine=refs/pull/748/head",
            "--setting-json",
            "fixture={\"id\":37}",
        ];
        let cli = Cli::parse_from(original);
        let args = original
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        let command = lab_offload_command(&cli.command)
            .expect("bench contract")
            .expect("portable bench");
        let decision =
            fixture_preflight_decision(&cli, Some("homeboy-lab"), "bench", Some(source.path()))
                .expect("placement decision");
        let handoff =
            materialize_generic_detached_lab_handoff(&args, source.path(), &command, decision)
                .expect("persist detached bench handoff");
        let persisted = agent_task_lifecycle::load_controller_plan(&handoff.run_id)
            .expect("durable replay plan");
        let replay = &persisted.metadata["generic_lab_command_replay"];
        let expected_args = portable_deferred_args(&args);

        assert_eq!(replay["normalized_args"], serde_json::json!(expected_args));
        assert_eq!(replay["lab_command"], serde_json::json!(command));
        assert_eq!(
            replay["inputs"]["options"]["rig"],
            serde_json::json!(["ssi-fixtures"])
        );
        assert_eq!(
            replay["inputs"]["options"]["extension"],
            serde_json::json!(["blocks-engine=refs/pull/748/head"])
        );
        assert_eq!(
            replay["inputs"]["options"]["setting-json"],
            serde_json::json!(["fixture={\"id\":37}"])
        );
        assert_eq!(
            replay["materialization"]["canonical_root"],
            serde_json::json!(source.path().canonicalize().expect("canonical source"))
        );
        assert!(replay["materialization"]["content_identity"]
            .as_str()
            .is_some_and(|identity| !identity.is_empty()));

        agent_task_lifecycle::record_pre_execution_failure(
            &handoff.run_id,
            &persisted,
            "lab_daemon_admission",
            &Error::internal_unexpected("daemon unavailable"),
        )
        .expect("persist pre-provider failure");
        let retry_args = [
            "homeboy",
            "agent-task",
            "retry",
            "bench-pre-provider-failure",
            "--run",
            "--new-run-id",
            "bench-pre-provider-failure-retry1",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let retry_cli = Cli::parse_from(&retry_args);
        let first = materialize_agent_task_retry_handoff(&retry_cli, &retry_args)
            .expect("materialize retry")
            .expect("retry handoff");
        let second = materialize_agent_task_retry_handoff(&retry_cli, &retry_args)
            .expect("idempotently rematerialize retry")
            .expect("retry handoff");

        assert!(first.replays_generic_command);
        assert_eq!(first.run_id, "bench-pre-provider-failure-retry1");
        assert_eq!(first.args, expected_args);
        assert_eq!(second.args, first.args);
        assert_eq!(second.run_id, first.run_id);
        assert_eq!(
            first.primary_workspace,
            source.path().canonicalize().expect("canonical workspace")
        );
        assert_eq!(
            first.plan.metadata["generic_lab_command_replay"],
            persisted.metadata["generic_lab_command_replay"]
        );
    });
}

#[test]
fn generic_lab_replay_retry_rejects_a_changed_workspace_before_reserving_a_successor() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source workspace");
        git_init(source.path());
        let args = vec![
            "homeboy".to_string(),
            "--placement".to_string(),
            "lab".to_string(),
            "--detach-after-handoff".to_string(),
            "bench".to_string(),
            "blocks-engine".to_string(),
            "--run-id".to_string(),
            "changed-generic-replay".to_string(),
        ];
        let cli = Cli::parse_from(&args);
        let command = lab_offload_command(&cli.command)
            .expect("bench contract")
            .expect("portable bench");
        let decision = placement_decision(&cli, Some("homeboy-lab"), "bench", Some(source.path()))
            .expect("placement decision");
        let handoff =
            materialize_generic_detached_lab_handoff(&args, source.path(), &command, decision)
                .expect("persist detached bench handoff");
        agent_task_lifecycle::record_pre_execution_failure(
            &handoff.run_id,
            &handoff.plan,
            "lab_daemon_admission",
            &Error::internal_unexpected("daemon unavailable"),
        )
        .expect("persist pre-provider failure");
        std::fs::write(source.path().join("changed.txt"), "changed\n").expect("change workspace");
        let retry_args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "retry".to_string(),
            handoff.run_id.clone(),
            "--run".to_string(),
            "--new-run-id".to_string(),
            "changed-generic-replay-retry".to_string(),
        ];
        let handoff =
            materialize_agent_task_retry_handoff(&Cli::parse_from(&retry_args), &retry_args)
                .expect("controller preflight validates only replay contract shape")
                .expect("retry handoff");
        assert!(handoff.replays_generic_command);
        assert_eq!(
            agent_task_lifecycle::run_record_exists("changed-generic-replay-retry")
                .expect("check retry reservation"),
            true,
            "runner-aware staging owns the content comparison"
        );
    });
}

#[test]
fn generic_lab_replay_retry_rejects_a_missing_workspace_before_reserving_a_successor() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source workspace");
        git_init(source.path());
        let args = vec![
            "homeboy".to_string(),
            "--placement".to_string(),
            "lab".to_string(),
            "--detach-after-handoff".to_string(),
            "bench".to_string(),
            "blocks-engine".to_string(),
            "--run-id".to_string(),
            "missing-generic-replay".to_string(),
        ];
        let cli = Cli::parse_from(&args);
        let command = lab_offload_command(&cli.command)
            .expect("bench contract")
            .expect("portable bench");
        let decision = placement_decision(&cli, Some("homeboy-lab"), "bench", Some(source.path()))
            .expect("placement decision");
        let handoff =
            materialize_generic_detached_lab_handoff(&args, source.path(), &command, decision)
                .expect("persist detached bench handoff");
        agent_task_lifecycle::record_pre_execution_failure(
            &handoff.run_id,
            &handoff.plan,
            "lab_daemon_admission",
            &Error::internal_unexpected("daemon unavailable"),
        )
        .expect("persist pre-provider failure");
        std::fs::remove_dir_all(source.path()).expect("remove workspace");
        let retry_args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "retry".to_string(),
            handoff.run_id.clone(),
            "--run".to_string(),
            "--new-run-id".to_string(),
            "missing-generic-replay-retry".to_string(),
        ];
        let handoff =
            materialize_agent_task_retry_handoff(&Cli::parse_from(&retry_args), &retry_args)
                .expect("controller preflight validates only replay contract shape")
                .expect("retry handoff");
        assert!(handoff.replays_generic_command);
        assert_eq!(
            agent_task_lifecycle::run_record_exists("missing-generic-replay-retry")
                .expect("check retry reservation"),
            true,
            "runner-aware staging owns workspace availability and identity"
        );
    });
}

#[test]
fn generic_lab_replay_legacy_identity_fails_closed_before_execution() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source workspace");
        git_init(source.path());
        let cli = Cli::parse_from(["homeboy", "bench", "blocks-engine"]);
        let command = lab_offload_command(&cli.command)
            .expect("bench contract")
            .expect("portable bench");
        let decision = placement_decision(&cli, Some("homeboy-lab"), "bench", Some(source.path()))
            .expect("placement decision");
        let handoff = materialize_generic_detached_lab_handoff(
            &[
                "homeboy".to_string(),
                "bench".to_string(),
                "blocks-engine".to_string(),
            ],
            source.path(),
            &command,
            decision,
        )
        .expect("persist replay plan");
        let mut legacy = handoff.plan;
        legacy.metadata["generic_lab_command_replay"]["materialization"]["content_identity"] =
            serde_json::json!("snapshot:legacy");

        let error = validate_generic_lab_command_replay_workspace(&legacy)
            .expect_err("legacy identity must not reach replay execution");
        assert!(error.message.contains("legacy Lab replay identity"));
    });
}

#[test]
fn explicit_local_promotion_defers_target_resolution_to_promotion() {
    let args = [
        "homeboy",
        "--placement",
        "local",
        "agent-task",
        "promote",
        "/tmp/aggregate.json",
        "--to-worktree",
        "fixture@dirty-candidate",
    ];
    let cli = Cli::parse_from(args);
    let normalized = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        route_after_parse_with_provenance(&cli, &normalized, None, None).unwrap(),
        None
    );
}

#[test]
fn cook_preview_bypasses_every_placement_route_without_durable_state() {
    crate::test_support::with_isolated_home(|home| {
        for placement in [
            vec!["--placement", "local"],
            vec!["--placement", "auto"],
            vec!["--placement", "lab"],
            vec!["--runner", "homeboy-lab"],
        ] {
            let mut args = vec!["homeboy"];
            args.extend(placement.iter().copied());
            args.extend([
                "agent-task",
                "cook",
                "--preview",
                "--prompt",
                "inspect the task",
                "--to-worktree",
                "fixture@preview",
                "--verify",
                "true",
            ]);
            let normalized = args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>();
            let cli = Cli::parse_from(&normalized);
            let before = std::fs::read_dir(home).expect("read isolated home").count();

            assert_eq!(
                route_after_parse_with_provenance(&cli, &normalized, None, None)
                    .expect("preview bypasses placement"),
                None,
                "{placement:?}"
            );
            assert_eq!(
                std::fs::read_dir(home).expect("read isolated home").count(),
                before,
                "{placement:?} preview created durable routing state"
            );
        }
    });
}
use clap::Parser;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn review_test_inlines_external_database_service_profile_before_lab_handoff() {
    let profile = tempdir().expect("profile directory");
    let settings = profile.path().join("phpunit-db-service.json");
    fs::write(
        &settings,
        r#"{"database_service":{"host":"127.0.0.1","port":3306,"user":"root"}}"#,
    )
    .expect("write settings profile");
    let cli = Cli::parse_from([
        "homeboy",
        "review",
        "test",
        "fixture",
        "--settings-json-file",
        settings.to_str().expect("utf8 settings path"),
        "--setting-json",
        "database_service={\"host\":\"db.lab\"}",
    ]);
    let args = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "test".to_string(),
        "fixture".to_string(),
        "--settings-json-file".to_string(),
        settings.to_string_lossy().to_string(),
        "--setting-json".to_string(),
        "database_service={\"host\":\"db.lab\"}".to_string(),
    ];

    let routed = inline_portable_settings_profiles(&cli, &args).expect("portable settings args");

    assert!(!routed.iter().any(|arg| arg == "--settings-json-file"));
    let profile_setting = routed
        .windows(2)
        .find(|pair| pair[0] == "--setting-json" && pair[1].starts_with("database_service="))
        .expect("profile converted to a typed setting");
    assert!(profile_setting[1].contains("127.0.0.1"));
    assert!(
        routed
            .iter()
            .position(|arg| arg == &profile_setting[1])
            .expect("profile setting index")
            < routed
                .iter()
                .rposition(|arg| arg == "database_service={\"host\":\"db.lab\"}")
                .expect("explicit setting index")
    );
}

#[test]
fn credential_profile_is_rejected_without_persisting_plaintext() {
    let profile = tempdir().expect("profile directory");
    let settings = profile.path().join("credentials.json");
    fs::write(
        &settings,
        r#"{"database_service":{"host":"db.lab","password":"fixture-password"}}"#,
    )
    .expect("write settings profile");
    let cli = Cli::parse_from([
        "homeboy",
        "review",
        "test",
        "fixture",
        "--settings-json-file",
        settings.to_str().expect("utf8 settings path"),
    ]);
    let args = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "test".to_string(),
        "fixture".to_string(),
        "--settings-json-file".to_string(),
        settings.to_string_lossy().to_string(),
    ];

    let error =
        inline_portable_settings_profiles(&cli, &args).expect_err("credential profile refused");

    assert!(error.message.contains("database_service.password"));
    assert!(error.message.contains("cannot be inlined"));
    assert!(error.message.contains("--runner-secret-env"));
    assert!(!error.to_string().contains("fixture-password"));
}

#[test]
fn deferred_workload_command_is_portable_and_omits_controller_placement() {
    let portable = portable_deferred_args(&[
        "homeboy".to_string(),
        "--placement".to_string(),
        "lab-or-local".to_string(),
        "review".to_string(),
        "test".to_string(),
        "fixture".to_string(),
    ]);

    assert_eq!(
        portable,
        vec!["homeboy", "review", "test", "fixture"],
        "the worker supplies its selected runner at replay time"
    );
}

#[test]
fn lab_cook_dispatcher_recipe_round_trips_exact_transport() {
    let dispatcher = LabCookAttemptDispatcher {
        runner_id: "homeboy-lab".to_string(),
        placement_decision: fixture_preflight_decision(
            &Cli::parse_from(["homeboy", "status"]),
            Some("homeboy-lab"),
            "test-cook-dispatch",
            None,
        )
        .expect("test placement decision"),
        allow_local_fallback: true,
        allow_dirty_lab_workspace: false,
        skip_deps_hydration: true,
        detach_after_handoff: true,
        source_path: Some(PathBuf::from("/controller/source")),
        job_overrides: runners::LabJobOverrides {
            env: [("MODE".to_string(), "test".to_string())].into(),
            secret_env_names: vec!["TOKEN".to_string()],
            workspace_root: Some("/runner/workspaces".to_string()),
        },
        progress_reporter: crate::commands::agent_task::CookProgressReporter::new(false),
    };
    let recipe = crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher::durable_recipe(
        &dispatcher,
    )
    .unwrap();

    let reconstructed = reconstruct_cook_attempt_dispatcher(&recipe)
        .unwrap()
        .expect("Lab dispatcher reconstructed");

    assert_eq!(reconstructed.durable_recipe().unwrap(), recipe);

    let mut legacy_recipe = recipe;
    legacy_recipe
        .as_object_mut()
        .expect("dispatcher recipe")
        .remove("detach_after_handoff");
    let legacy = reconstruct_cook_attempt_dispatcher(&legacy_recipe)
        .unwrap()
        .expect("legacy Lab dispatcher reconstructed");
    assert_eq!(
        legacy.durable_recipe().unwrap()["detach_after_handoff"],
        false
    );
}

#[test]
fn unchanged_attempt_inputs_preserve_the_canonical_decision() {
    let cli = Cli::parse_from(["homeboy", "--runner", "homeboy-lab", "status"]);
    let first = tempdir().expect("first child workspace");
    let initial =
        fixture_preflight_decision(&cli, Some("homeboy-lab"), "child-a", Some(first.path()))
            .expect("initial child decision");

    let preserved =
        finalize_replacement_attempt(&initial, "homeboy-lab", "child-a", Some(first.path()));

    assert_eq!(initial.decision_id, preserved.decision_id);
    assert_eq!(
        preserved.identity.workspace,
        first.path().display().to_string()
    );
    assert_eq!(preserved.identity.task, "child-a");
    assert_eq!(
        preserved
            .runner
            .as_ref()
            .map(|runner| runner.runner_id.as_str()),
        Some("homeboy-lab")
    );
}

#[test]
fn durable_placement_identity_survives_workspace_exception_retry_continuation_and_fanout() {
    crate::test_support::with_isolated_home(|_| {
        let workspace_parent = tempdir().expect("workspace parent");
        let replacement_parent = tempdir().expect("replacement workspace parent");
        let workspace = workspace_parent.path().join("candidate");
        let replacement_workspace = replacement_parent.path().join("candidate");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&replacement_workspace).expect("replacement workspace");
        let cli = Cli::parse_from(["homeboy", "--runner", "homeboy-lab", "status"]);
        let initial = fixture_preflight_decision(
            &cli,
            Some("homeboy-lab"),
            "provider-task",
            Some(&workspace),
        )
        .expect("initial placement decision");
        let template = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "placement-e2e",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "provider-task",
                "executor": { "backend": "fixture" },
                "instructions": "exercise durable placement"
            }))
            .expect("task")],
        );

        // These paths re-enter staging with the same declared workspace. They
        // must retain one decision and attach outcomes to that exact identity.
        for run_id in [
            "workspace-exception",
            "workspace-retry",
            "workspace-continuation",
            "fanout-child-a",
            "fanout-child-b",
        ] {
            let mut plan = template.clone();
            let decision = resolve_cook_attempt_placement_decision(
                &mut plan,
                run_id,
                &initial,
                "homeboy-lab",
                "provider-task",
                Some(&workspace),
            )
            .expect("preserve placement decision");
            assert_eq!(decision.decision_id, initial.decision_id, "{run_id}");
            assert!(plan
                .metadata
                .get("execution_placement_invalidated")
                .is_none());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("persist attempt");
            homeboy_agents::agent_task_lifecycle::record_execution_placement_outcome(
                run_id,
                decision
                    .outcome(
                        homeboy_lab_runner_contract::EffectiveExecutionPlacement::Lab,
                        Some("homeboy-lab".to_string()),
                    )
                    .expect("authorized Lab outcome"),
            )
            .expect("persist verified outcome");
            let record = agent_task_lifecycle::reconcile_status(run_id).expect("durable attempt");
            assert_eq!(
                record.metadata["execution_placement_decision"]["decision_id"],
                initial.decision_id
            );
            assert_eq!(
                record.metadata["execution_placement_outcome"]["decision_id"],
                initial.decision_id
            );
        }

        // A replacement workspace is the declared transition where the old
        // identity becomes stale. Its outcome must bind to the replacement.
        let mut replacement_plan = template;
        replacement_plan.metadata = serde_json::json!({
            "execution_placement_decision": initial,
        });
        let replacement = resolve_cook_attempt_placement_decision(
            &mut replacement_plan,
            "workspace-replacement",
            &fixture_preflight_decision(
                &cli,
                Some("homeboy-lab"),
                "provider-task",
                Some(&workspace),
            )
            .expect("initial decision"),
            "homeboy-lab",
            "provider-task",
            Some(&replacement_workspace),
        )
        .expect("replace stale decision");
        assert_ne!(
            replacement.decision_id,
            replacement_plan.metadata["execution_placement_invalidated"]["prior_decision_id"]
        );
        assert_eq!(
            replacement_plan.metadata["execution_placement_invalidated"]["reasons"],
            serde_json::json!(["workspace_changed"])
        );
        agent_task_lifecycle::submit_plan(&replacement_plan, Some("workspace-replacement"))
            .expect("persist replacement");
        let replacement_record = agent_task_lifecycle::reconcile_status("workspace-replacement")
            .expect("replacement record");
        assert_eq!(
            replacement_record.metadata["execution_placement_invalidated"]["prior_decision_id"],
            replacement_plan.metadata["execution_placement_invalidated"]["prior_decision_id"]
        );
        assert_eq!(
            replacement_record.metadata["execution_placement_invalidated"]["reasons"],
            serde_json::json!(["workspace_changed"])
        );
        homeboy_agents::agent_task_lifecycle::record_execution_placement_outcome(
            "workspace-replacement",
            replacement
                .outcome(
                    homeboy_lab_runner_contract::EffectiveExecutionPlacement::Lab,
                    Some("homeboy-lab".to_string()),
                )
                .expect("replacement Lab outcome"),
        )
        .expect("persist replacement outcome");
        let replacement_record = agent_task_lifecycle::reconcile_status("workspace-replacement")
            .expect("replacement record");
        assert_eq!(
            replacement_record.metadata["execution_placement_outcome"]["decision_id"],
            replacement.decision_id
        );
    });
}

#[test]
fn command_placement_matrix_separates_preferred_lab_from_required_lab() {
    let source = tempdir().expect("source workspace");
    let cases = [
        (
            vec!["homeboy", "--placement", "local", "review", "lint"],
            None,
            homeboy::cli_surface::Placement::Local,
            homeboy_lab_runner_contract::ExecutionPlacementRequirement::Either,
            true,
        ),
        (
            vec!["homeboy", "review", "lint"],
            Some("policy-lab"),
            homeboy::cli_surface::Placement::Auto,
            homeboy_lab_runner_contract::ExecutionPlacementRequirement::Either,
            true,
        ),
        (
            vec!["homeboy", "--placement", "lab-or-local", "review", "lint"],
            Some("policy-lab"),
            homeboy::cli_surface::Placement::LabOrLocal,
            homeboy_lab_runner_contract::ExecutionPlacementRequirement::Either,
            true,
        ),
        (
            vec!["homeboy", "--runner", "pinned-lab", "review", "lint"],
            Some("pinned-lab"),
            homeboy::cli_surface::Placement::Auto,
            homeboy_lab_runner_contract::ExecutionPlacementRequirement::Lab,
            false,
        ),
        (
            vec!["homeboy", "--placement", "lab", "review", "lint"],
            Some("policy-lab"),
            homeboy::cli_surface::Placement::Lab,
            homeboy_lab_runner_contract::ExecutionPlacementRequirement::Lab,
            false,
        ),
    ];

    for (args, runner, placement, required, local_fallback) in cases {
        let cli = Cli::parse_from(&args);
        let decision = fixture_preflight_decision(&cli, runner, "matrix", Some(source.path()))
            .expect("placement decision");

        assert_eq!(cli.placement, placement);
        assert_eq!(decision.required, required);
        assert_eq!(
            decision
                .verifies_outcome(homeboy_lab_runner_contract::EffectiveExecutionPlacement::Local),
            local_fallback,
            "{args:?} local fallback authorization"
        );
        assert_eq!(
            decision.runner.as_ref().map(|runner| runner.source),
            runner.map(|_| {
                if cli.runner.is_some() {
                    homeboy_lab_runner_contract::RunnerSelectionSource::Explicit
                } else {
                    homeboy_lab_runner_contract::RunnerSelectionSource::Policy
                }
            })
        );
    }
}

#[test]
fn lab_cook_child_invocation_consumes_controller_runner_placement() {
    let args = lab_cook_attempt_args("{\"tasks\":[]}".to_string(), "cook-attempt-1");

    assert_eq!(
        args,
        vec![
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "run-plan",
            "--plan",
            "{\"tasks\":[]}",
            "--record-run-id",
            "cook-attempt-1",
        ]
    );
    assert!(!args.iter().any(|arg| arg == "--runner"));
    assert!(!args.iter().any(|arg| arg.starts_with("--runner=")));
}

#[test]
fn cook_dispatch_stages_runner_identity_without_starting_handoff_lease() {
    crate::test_support::with_isolated_home(|_| {
        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "cook-preacceptance-order",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "task",
                "executor": { "backend": "fixture" },
                "instructions": "exercise controller handoff staging"
            }))
            .expect("task")],
        );
        let dispatcher = LabCookAttemptDispatcher {
            runner_id: "missing-homeboy-lab".to_string(),
            placement_decision: fixture_preflight_decision(
                &Cli::parse_from(["homeboy", "status"]),
                Some("missing-homeboy-lab"),
                "task",
                None,
            )
            .expect("test placement decision"),
            allow_local_fallback: false,
            allow_dirty_lab_workspace: false,
            skip_deps_hydration: false,
            detach_after_handoff: false,
            source_path: None,
            job_overrides: runners::LabJobOverrides::default(),
            progress_reporter: crate::commands::agent_task::CookProgressReporter::new(false),
        };

        let error =
            crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher::dispatch_attempt(
                &dispatcher,
                plan,
                "cook-preacceptance-order",
                None,
            )
            .expect_err("missing Lab target rejects after controller staging");

        assert!(!error.message.is_empty());
        assert_eq!(error.retryable, Some(true));
        let record = agent_task_lifecycle::reconcile_status("cook-preacceptance-order")
            .expect("controller record remains inspectable after preacceptance failure");
        assert!(record.lab_handoff.is_none());
        assert_eq!(record.metadata["runner_id"], "missing-homeboy-lab");
        assert_eq!(
            record.metadata["runner_execution_record"]["status"],
            "planned"
        );
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "lab_handoff_preacceptance"
        );
        assert_eq!(record.metadata["pre_execution_failure"]["retryable"], true);
        assert_eq!(
            record.metadata["pre_execution_failure"]["failure_classification"],
            "transient"
        );
        assert_eq!(
            record.metadata["pre_execution_failure"]["provider_executions_consumed"],
            0
        );
        assert!(record
            .metadata
            .get("execution_placement_invalidated")
            .is_none());
        assert_eq!(
            record.metadata["execution_placement_decision"]["identity"]["task"],
            "task"
        );
    });
}

#[test]
fn changed_scope_lint_is_lab_portable() {
    let cli = Cli::parse_from([
        "homeboy",
        "review",
        "lint",
        "--changed-since",
        "origin/main",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "review lint");
    assert!(command.is_portable());
}

#[test]
fn nested_review_quality_subcommands_use_specific_lab_labels() {
    for (args, expected_label) in [
        (
            vec!["homeboy", "review", "audit", "data-machine"],
            "review audit",
        ),
        (
            vec!["homeboy", "review", "lint", "data-machine"],
            "review lint",
        ),
        (
            vec!["homeboy", "review", "test", "data-machine"],
            "review test",
        ),
        (
            vec!["homeboy", "review", "build", "data-machine"],
            "review build",
        ),
        (
            vec![
                "homeboy",
                "review",
                "ci",
                "run",
                "data-machine",
                "--job",
                "lint",
            ],
            "review ci",
        ),
    ] {
        let cli = Cli::parse_from(args);
        let command = cli.command.lab_contract().unwrap();

        assert_eq!(command.hot_label, expected_label);
    }
}

#[test]
fn nested_review_lint_dispatch_uses_matching_lab_label() {
    let cli = Cli::parse_from(["homeboy", "review", "lint", "data-machine"]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "review lint");
    assert!(command.is_portable());
}

#[test]
fn nested_review_quality_subcommand_resolves_effective_component() {
    let cli = Cli::parse_from(["homeboy", "review", "lint", "data-machine"]);
    let Commands::Review(args) = cli.command else {
        panic!("expected review command");
    };

    assert_eq!(
        args.effective_component_args().component.as_deref(),
        Some("data-machine")
    );
}

#[test]
fn nested_review_quality_in_dir_offload_uses_current_dir_path() {
    let dir = tempdir().expect("tempdir");
    let _cwd = CwdGuard::set(dir.path());
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let rewritten = lab_route_source_path_args(&cli.command, &normalized, false)
        .expect("review lint without component gets cwd path rewrite");
    let cwd = std::env::current_dir().expect("current dir");

    assert_eq!(rewritten[0..3], normalized);
    assert_eq!(rewritten[3], "--path");
    assert_eq!(rewritten[4], cwd.to_string_lossy());
}

#[test]
fn explicit_runner_for_changed_scope_test_is_lab_portable() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "review",
        "test",
        "--changed-since",
        "origin/main",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert_eq!(command.hot_label, "review test");
    assert!(command.is_portable());
}

#[test]
fn destructive_fuzz_local_execution_requires_explicit_destructive_local_override() {
    // Serialize against LAB_OFFLOAD_METADATA_ENV-asserting tests: this
    // routes through route_after_parse, which mutates that global.
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let normalized = vec![
        "homeboy",
        "--placement",
        "local",
        "fuzz",
        "run",
        "component-a",
        "--allow-destructive",
        "--isolation",
        "isolated",
        "--isolation-proof",
        "proof.json",
    ];
    let cli = Cli::parse_from(&normalized);

    assert!(destructive_fuzz_requires_lab(&cli.command));

    let error = crate::test_support::with_isolated_home(|_| {
        route_after_parse_with_provenance(
            &cli,
            &normalized
                .iter()
                .map(|arg| arg.to_string())
                .collect::<Vec<_>>(),
            None,
            None,
        )
        .expect_err("destructive fuzz local route should be refused")
    });
    assert!(error
        .to_string()
        .contains("destructive fuzz refused local controller execution"));
}

#[test]
fn destructive_fuzz_local_override_is_command_specific_and_explicit() {
    let cli = Cli::parse_from([
        "homeboy",
        "--placement",
        "local",
        "fuzz",
        "run",
        "component-a",
        "--allow-destructive",
        "--allow-local-destructive-fuzz",
        "--isolation",
        "isolated",
        "--isolation-proof",
        "proof.json",
    ]);

    assert!(!destructive_fuzz_requires_lab(&cli.command));
}

#[test]
fn rig_up_dry_run_with_runner_emits_runner_exec_plan() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    crate::test_support::with_isolated_home(|home| {
        runners::create(
            r#"{"id":"homeboy-lab","kind":"local","homeboy_path":"/runner/bin/homeboy-patched"}"#,
            false,
        )
        .expect("runner");
        write_command_only_rig(home.path(), "script-matrix");
        let output = home.path().join("plan.json");
        let normalized = vec![
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "rig".to_string(),
            "up".to_string(),
            "script-matrix".to_string(),
            "--dry-run".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse_with_provenance(
            &cli,
            &normalized,
            Some(&output.to_string_lossy()),
            None,
        )
        .expect("route rig up plan");

        assert_eq!(outcome, Some(0));
        let plan: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output).expect("read output plan"))
                .expect("parse output plan");
        assert_eq!(plan["variant"], "up_plan");
        assert_eq!(plan["payload"]["runner_id"], "homeboy-lab");
        assert_eq!(
            plan["payload"]["selected_homeboy_binary"],
            "/runner/bin/homeboy-patched"
        );
        assert_eq!(
            plan["payload"]["commands"][0],
            "/runner/bin/homeboy-patched runner exec homeboy-lab --cwd tools --env MATRIX=portable -- sh -c ./scripts/run-matrix.sh"
        );
    });
}

#[test]
fn lab_job_overrides_parse_env_json_and_workspace_root() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner-env",
        "STUDIO_NATIVE_TRACE_SAMPLE_RUNTIME_PLUGIN_PATH=/tmp/sample-runtime",
        "--runner-secret-env",
        "API_TOKEN",
        "--lab-env-json",
        r#"{"EXTRA_PATH":"/tmp/extra","EMPTY":null}"#,
        "--runner-workspace-root",
        "/srv/job-workspace",
        "review",
    ]);

    let overrides = lab_job_overrides(&cli).expect("overrides");

    assert_eq!(
        overrides.env["STUDIO_NATIVE_TRACE_SAMPLE_RUNTIME_PLUGIN_PATH"],
        "/tmp/sample-runtime"
    );
    assert_eq!(overrides.env["EXTRA_PATH"], "/tmp/extra");
    assert_eq!(overrides.env["EMPTY"], "");
    assert_eq!(
        overrides.workspace_root.as_deref(),
        Some("/srv/job-workspace")
    );
    assert!(overrides
        .secret_env_names
        .contains(&"API_TOKEN".to_string()));
}

#[test]
fn lab_job_overrides_reject_sensitive_runner_env_values() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner-env",
        "API_TOKEN=secret-token",
        "review",
    ]);

    let error = lab_job_overrides(&cli).expect_err("plaintext runner secret is refused");

    assert_eq!(error.details["field"], "runner-env");
    assert!(error.message.contains("--runner-secret-env API_TOKEN"));
}

#[test]
fn lab_job_overrides_reject_invalid_env_shapes() {
    let cli = Cli::parse_from(["homeboy", "--runner-env", "NO_EQUALS", "review"]);
    let err = lab_job_overrides(&cli).expect_err("invalid pair");
    assert_eq!(err.code.as_str(), "validation.invalid_argument");

    let cli = Cli::parse_from(["homeboy", "--lab-env-json", "[]", "review"]);
    let err = lab_job_overrides(&cli).expect_err("invalid json object");
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
}

#[test]
fn deferred_plan_persists_public_env_and_runner_secret_identity_without_plaintext() {
    crate::test_support::with_isolated_home(|_| {
        let input = homeboy::deferred_workload::DeferredWorkloadInput {
            command_label: "review test".to_string(),
            args: vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            placement: "auto".to_string(),
            resource_requirement: "eligible_lab_runner".to_string(),
            portability: "portable_lab_route".to_string(),
            reason: "fixture".to_string(),
            ci_alternative: "CI".to_string(),
            resolved_contract: serde_json::json!({}),
            resolved_resources: serde_json::json!({}),
            test_requirements: homeboy::deferred_workload::DeferredWorkloadRequirements {
                required_runtimes: ["homeboy".to_string()].into(),
                required_capabilities: Default::default(),
            },
            source_directory: None,
            job_overrides: runners::LabJobOverrides {
                env: [
                    ("DB_SERVICE_HOST".to_string(), "db.fixture".to_string()),
                    ("DB_SERVICE_PORT".to_string(), "3306".to_string()),
                ]
                .into(),
                secret_env_names: vec!["DB_SERVICE_PASSWORD".to_string()],
                workspace_root: None,
            },
        };
        let deferred = homeboy::deferred_workload::defer(input).expect("defer fixture");
        assert_eq!(deferred.job_overrides.env["DB_SERVICE_HOST"], "db.fixture");
        assert_eq!(deferred.job_overrides.env["DB_SERVICE_PORT"], "3306");
        assert!(!deferred
            .job_overrides
            .env
            .contains_key("DB_SERVICE_PASSWORD"));
        assert_eq!(
            deferred.job_overrides.secret_env_names,
            ["DB_SERVICE_PASSWORD"]
        );
        let durable_json = serde_json::to_string(&deferred).expect("deferred JSON");
        let status_json = serde_json::to_string(
            &crate::commands::deferred_workload::redacted_record(&deferred),
        )
        .expect("status JSON");
        for json in [&durable_json, &status_json] {
            assert!(!json.contains("fixture-password"));
            assert!(!json.contains("DB_SERVICE_PASSWORD\":\""));
            assert!(json.contains("DB_SERVICE_PASSWORD"));
        }
    });
}

#[test]
fn deferred_status_redacts_all_settings_arguments() {
    let mut record = homeboy::deferred_workload::DeferredWorkload {
        id: "deferred-settings".to_string(),
        fingerprint: "fixture".to_string(),
        command_label: "review test".to_string(),
        args: vec![
            "homeboy".to_string(),
            "review".to_string(),
            "test".to_string(),
            "--setting".to_string(),
            "mode=debug".to_string(),
            "--setting-json=database_service={\"password\":\"fixture-password\"}".to_string(),
        ],
        placement: "auto".to_string(),
        resource_requirement: "eligible_lab_runner".to_string(),
        portability: "portable_lab_route".to_string(),
        reason: "fixture".to_string(),
        ci_alternative: "CI".to_string(),
        resolved_contract: serde_json::json!({}),
        resolved_resources: serde_json::json!({}),
        test_requirements: homeboy::deferred_workload::DeferredWorkloadRequirements {
            required_runtimes: ["homeboy".to_string()].into(),
            required_capabilities: Default::default(),
        },
        source_directory: None,
        job_overrides: Default::default(),
        state: homeboy::deferred_workload::DeferredWorkloadState::Deferred,
        created_at_ms: 0,
        updated_at_ms: 0,
        runner_id: None,
        claim_owner: None,
        claim_expires_at_ms: None,
    };
    record
        .job_overrides
        .secret_env_names
        .push("DB_SERVICE_PASSWORD".to_string());

    let status = crate::commands::deferred_workload::redacted_record(&record).to_string();

    assert!(!status.contains("mode=debug"));
    assert!(!status.contains("fixture-password"));
    assert!(status.contains("[REDACTED]"));
}

#[test]
fn manifest_resolved_portable_db_service_warm_defers_and_dispatches_secret_identity() {
    crate::test_support::with_isolated_home(|home| {
        let component = tempdir().expect("component directory");
        fs::write(
            component.path().join("homeboy.json"),
            r#"{"id":"portable-db-consumer","extensions":{"portable-db-service":{}}}"#,
        )
        .expect("component manifest");
        let extension_dir = home
            .path()
            .join(".config/homeboy/extensions/portable-db-service");
        fs::create_dir_all(&extension_dir).expect("extension directory");
        fs::write(
            extension_dir.join("portable-db-service.json"),
            include_str!(
                "../../../../../../../tests/fixtures/extension_manifests/portable-db-service.json"
            ),
        )
        .expect("portable DB service manifest");
        let _env = EnvGuard::set_many(&[
            ("DB_SERVICE_HOST", Some("db.fixture")),
            ("DB_SERVICE_PORT", Some("3306")),
            ("DB_SERVICE_PASSWORD", Some("fixture-password")),
        ]);
        let path = component.path().to_str().expect("component path");
        let cli = Cli::parse_from([
            "homeboy",
            "review",
            "test",
            "--path",
            path,
            "--runner-secret-env",
            "DB_SERVICE_PASSWORD",
        ]);

        let overrides = lab_job_overrides(&cli).expect("manifest-resolved overrides");
        assert_eq!(overrides.env["DB_SERVICE_HOST"], "db.fixture");
        assert_eq!(overrides.env["DB_SERVICE_PORT"], "3306");
        assert_eq!(overrides.secret_env_names, ["DB_SERVICE_PASSWORD"]);
        let normalized = vec![
            "homeboy".to_string(),
            "review".to_string(),
            "test".to_string(),
        ];
        let preflight = homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult::new(
            normalized.clone(),
            resource_policy::parsed_command_preflight_input(&cli, &normalized),
            None,
            None,
            homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::Defer,
            homeboy::core::parsed_command_preflight::FallbackDirective::None,
            crate::cli_runtime::placement_directive(&cli, None, false),
            None,
        );
        let deferred = homeboy::deferred_workload::defer(
            deferred_workload_input(
                &cli,
                &normalized,
                &preflight,
                review_test_deferred_requirements(&cli).expect("review test requirements"),
            )
            .expect("deferred input"),
        )
        .expect("warm deferral");
        let durable = serde_json::to_string(&deferred).expect("durable deferred record");
        assert!(!durable.contains("fixture-password"));

        let ready = std::cell::Cell::new(false);
        let ready_after_wait = &ready;
        crate::commands::deferred_workload::run_worker_with(
            &homeboy::core::paths::homeboy().expect("config root"),
            "worker-token",
            || {
                Ok(ready.get().then(|| {
                    crate::commands::deferred_workload::RunnerCapabilityInventory {
                        runner_id: "compatible-runner".to_string(),
                        runtime_ids: ["homeboy".to_string()].into(),
                        capabilities: ["homeboy".to_string()].into(),
                    }
                }))
            },
            |record, runner_id, _| {
                assert_eq!(runner_id, "compatible-runner");
                assert_eq!(record.job_overrides.env["DB_SERVICE_HOST"], "db.fixture");
                assert_eq!(
                    record.job_overrides.secret_env_names,
                    ["DB_SERVICE_PASSWORD"]
                );
                assert!(!serde_json::to_string(record)
                    .expect("record JSON")
                    .contains("fixture-password"));
                Ok(true)
            },
            || 10,
            |_| ready_after_wait.set(true),
        )
        .expect("compatible runner dispatch");
        assert_eq!(
            homeboy::deferred_workload::records().expect("records")[0].state,
            homeboy::deferred_workload::DeferredWorkloadState::Dispatched
        );
    });
}

#[test]
fn deferred_runner_env_wins_over_later_manifest_ambient_value() {
    crate::test_support::with_isolated_home(|home| {
        let component = tempdir().expect("component directory");
        fs::write(
            component.path().join("homeboy.json"),
            r#"{"id":"portable-db-consumer","extensions":{"portable-db-service":{}}}"#,
        )
        .expect("component manifest");
        let extension_dir = home
            .path()
            .join(".config/homeboy/extensions/portable-db-service");
        fs::create_dir_all(&extension_dir).expect("extension directory");
        fs::write(
            extension_dir.join("portable-db-service.json"),
            include_str!(
                "../../../../../../../tests/fixtures/extension_manifests/portable-db-service.json"
            ),
        )
        .expect("portable DB service manifest");
        let _env = EnvGuard::set_many(&[
            ("DB_SERVICE_HOST", Some("B")),
            ("DB_SERVICE_PORT", Some("3306")),
        ]);
        let path = component.path().to_str().expect("component path");

        let replay_args = vec![
            "homeboy".to_string(),
            "--runner-env".to_string(),
            "DB_SERVICE_HOST=A".to_string(),
            "review".to_string(),
            "test".to_string(),
            "--path".to_string(),
            path.to_string(),
        ];
        let replay = Cli::parse_from(replay_args);
        let overrides = lab_job_overrides(&replay).expect("replay overrides");

        assert_eq!(overrides.env["DB_SERVICE_HOST"], "A");
        assert_eq!(overrides.env["DB_SERVICE_PORT"], "3306");
        assert_eq!(overrides.secret_env_names, ["DB_SERVICE_PASSWORD"]);
    });
}

#[test]
fn changed_since_lint_keeps_git_scope_for_lab_runner() {
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
        "--changed-since".to_string(),
        "origin/main".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let rewritten = inject_lab_changed_files(&cli.command, &normalized).unwrap();

    assert!(rewritten.is_none());
}

#[test]
fn changed_since_test_keeps_git_scope_for_lab_runner() {
    let normalized = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "review".to_string(),
        "test".to_string(),
        "--changed-since=origin/main".to_string(),
        "--skip-lint".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let rewritten = inject_lab_changed_files(&cli.command, &normalized).unwrap();

    assert!(rewritten.is_none());
}

#[test]
fn lab_offload_subprocess_skips_recursive_lab_routing() {
    let _env = EnvGuard::set(
        homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV,
        r#"{"status":"offloaded"}"#,
    );
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "trace",
        "--rig",
        "gutenberg-pattern-preview-assets",
        "gutenberg",
        "pattern-preview-assets",
    ]);
    let normalized = [
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "trace".to_string(),
        "--rig".to_string(),
        "gutenberg-pattern-preview-assets".to_string(),
        "gutenberg".to_string(),
        "pattern-preview-assets".to_string(),
    ];

    let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None).unwrap();

    assert_eq!(outcome, None);
}

#[test]
fn runner_hosted_bench_exec_skips_recursive_lab_routing_without_explicit_runner() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::runner::RUNNER_HOSTED_EXEC_ENV, Some("1")),
        (homeboy::runner::RUNNER_PLACEMENT_RESOLVED_ENV, Some("1")),
        (homeboy::runner::RUNNER_ID_ENV, Some("homeboy-lab")),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "--placement".to_string(),
        "local".to_string(),
        "bench".to_string(),
        "--extension".to_string(),
        "wordpress".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None)
        .expect("runner-hosted bench execution should stay local");

    assert_eq!(outcome, None);
}

#[test]
fn ambient_resolved_marker_cannot_bypass_explicit_lab_placement() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::runner::RUNNER_HOSTED_EXEC_ENV, None),
        (homeboy::runner::RUNNER_ID_ENV, None),
        (homeboy::runner::RUNNER_PLACEMENT_RESOLVED_ENV, Some("1")),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "--placement".to_string(),
        "lab".to_string(),
        "review".to_string(),
        "lint".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);
    homeboy::core::parsed_command_preflight::reset_captured_result_for_test();
    homeboy::core::parsed_command_preflight::capture_result(
        homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult::new(
            normalized.clone(),
            resource_policy::parsed_command_preflight_input(&cli, &normalized),
            None,
            None,
            homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::NotApplicable,
            homeboy::core::parsed_command_preflight::FallbackDirective::RequiredLabUnavailable,
            crate::cli_runtime::placement_directive(&cli, None, false),
            None,
        ),
    );

    let err = crate::test_support::with_isolated_home(|_| {
        route_after_parse_with_provenance(&cli, &normalized, None, None)
            .expect_err("ambient marker must not bypass required Lab placement")
    });

    assert_ne!(err.code.as_str(), "internal.unexpected");
}

#[test]
fn managed_runner_context_bypasses_auto_routing_once() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::runner::RUNNER_HOSTED_EXEC_ENV, Some("1")),
        (homeboy::runner::RUNNER_PLACEMENT_RESOLVED_ENV, Some("1")),
        (homeboy::runner::RUNNER_ID_ENV, Some("homeboy-lab")),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    assert_eq!(
        route_after_parse_with_provenance(&cli, &normalized, None, None).expect("managed context"),
        None
    );
}

#[test]
fn explicit_local_cook_skips_the_second_interception_pass() {
    let local = Cli::parse_from([
        "homeboy",
        "--placement",
        "local",
        "agent-task",
        "cook",
        "--prompt",
        "run locally",
    ]);
    assert!(!needs_provider_resolved_cook_interception(&local, None));

    let automatic = Cli::parse_from([
        "homeboy",
        "--placement",
        "auto",
        "agent-task",
        "cook",
        "--prompt",
        "resolve provider placement",
    ]);
    assert!(needs_provider_resolved_cook_interception(&automatic, None));
    assert!(needs_provider_resolved_cook_interception(
        &automatic,
        Some("ready-runner")
    ));
}

#[test]
fn runner_resident_run_plan_does_not_require_a_second_controller_session() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::runner::RUNNER_HOSTED_EXEC_ENV, None),
        (homeboy::runner::RUNNER_PLACEMENT_RESOLVED_ENV, None),
        (homeboy::runner::RUNNER_ID_ENV, None),
        (
            homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV,
            Some("homeboy-lab"),
        ),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "--placement".to_string(),
        "local".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        r#"{"plan_id":"handoff","tasks":[]}"#.to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    assert_eq!(
        route_after_parse_with_provenance(&cli, &normalized, None, None)
            .expect("runner-resident handoff stays local"),
        None
    );
}

#[test]
fn nested_command_targeting_parent_runner_does_not_require_a_second_controller_session() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::runner::RUNNER_HOSTED_EXEC_ENV, None),
        (homeboy::runner::RUNNER_PLACEMENT_RESOLVED_ENV, None),
        (homeboy::runner::RUNNER_ID_ENV, None),
        (
            homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV,
            Some("homeboy-lab"),
        ),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--to-worktree".to_string(),
        "homeboy@nested-runner-context".to_string(),
        "--prompt".to_string(),
        "run a nested workload".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    assert_eq!(
        route_after_parse_with_provenance(&cli, &normalized, None, None)
            .expect("runner-resident cook stays local"),
        None
    );
}

#[test]
fn managed_promotion_handoff_does_not_require_runner_side_artifact_hydration() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::runner::RUNNER_HOSTED_EXEC_ENV, Some("1")),
        (homeboy::runner::RUNNER_PLACEMENT_RESOLVED_ENV, Some("1")),
        (homeboy::runner::RUNNER_ID_ENV, Some("homeboy-lab")),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        // An explicit runner already selects Lab. `--placement` alongside it is
        // rejected by clap since #11829, and this fixture is about the runner.
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "agent-task".to_string(),
        "promote".to_string(),
        "agent-task-preserved-run".to_string(),
        "--to-worktree".to_string(),
        "homeboy@fix-preserved-candidate".to_string(),
        "--task-id".to_string(),
        "task-preserved-artifact".to_string(),
        "--artifact-id".to_string(),
        "patch-preserved-artifact".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    assert_eq!(
        route_after_parse_with_provenance(&cli, &normalized, None, None)
            .expect("managed promotion must execute on its authorized runner"),
        None
    );
}

#[test]
fn agent_task_doctor_runner_option_routes_locally() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let normalized = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "doctor".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "--repair".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));

    let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None)
        .expect("agent-task doctor owns --runner and should not be Lab-routed");

    assert_eq!(outcome, None);
    assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
}

#[test]
fn trace_lab_dispatch_timeout_reads_env_override() {
    let _env = EnvGuard::set(lab_routing::LAB_TRACE_DISPATCH_TIMEOUT_ENV, "7");

    assert_eq!(
        lab_routing::lab_trace_dispatch_timeout(),
        std::time::Duration::from_secs(7)
    );
}

#[test]
fn lab_route_dispatch_timeout_plumbs_core_timeout() {
    let trace_cli = Cli::parse_from(["homeboy", "trace", "list"]);
    let lint_cli = Cli::parse_from(["homeboy", "review", "lint"]);

    assert_eq!(
        lab_route_dispatch_timeout(&trace_cli.command),
        Some(lab_routing::lab_trace_dispatch_timeout())
    );
    assert_eq!(lab_route_dispatch_timeout(&lint_cli.command), None);
}

#[test]
fn explicit_runner_provider_discovery_dispatch_is_bounded() {
    // An explicit `--runner` provider read still has to answer inside a
    // caller's patience: it degrades to the labelled dispatch-timeout error
    // rather than waiting out the generic workload budget (#9763).
    let _env = EnvGuard::remove(lab_routing::LAB_PROVIDER_DISCOVERY_DISPATCH_TIMEOUT_ENV);
    let explicit = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "agent-task",
        "providers",
    ]);
    let unscoped = Cli::parse_from(["homeboy", "agent-task", "providers"]);
    let budget = lab_routing::lab_provider_discovery_dispatch_timeout();

    assert_eq!(lab_route_dispatch_timeout(&explicit.command), Some(budget));
    assert_eq!(lab_route_dispatch_timeout(&unscoped.command), Some(budget));
    assert!(
        budget < std::time::Duration::from_secs(lab_routing::DEFAULT_LAB_DISPATCH_TIMEOUT_SECS),
        "provider discovery must not inherit the generic workload dispatch budget"
    );
}

#[test]
fn provider_discovery_dispatch_timeout_reads_env_override() {
    let _env = EnvGuard::set(
        lab_routing::LAB_PROVIDER_DISCOVERY_DISPATCH_TIMEOUT_ENV,
        "11",
    );

    assert_eq!(
        lab_routing::lab_provider_discovery_dispatch_timeout(),
        std::time::Duration::from_secs(11)
    );
}

#[test]
fn detached_agent_task_handoffs_do_not_use_trace_dispatch_timeout() {
    let cook = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--repo",
        "homeboy",
        "--goal",
        "Fix the detached handoff",
        "--to-worktree",
        "homeboy@fix-7971",
        "--run-id",
        "cook-7971",
        "--runner",
        "homeboy-lab",
    ]);
    let batch = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "fanout",
        "cook-batch",
        "--repo",
        "homeboy",
        "--verify",
        "cargo test --lib",
        "--run-plan",
        "https://github.com/Extra-Chill/homeboy/issues/7167",
    ]);
    let retry = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "retry",
        "failed-run",
        "--run",
        "--runner",
        "homeboy-lab",
    ]);

    for cli in [&cook, &batch, &retry] {
        assert_eq!(lab_route_dispatch_timeout(&cli.command), None);
    }
}

#[test]
fn cook_retry_lab_source_is_the_derived_baseline_not_the_controller_workspace() {
    let baseline = tempfile::tempdir().expect("baseline");
    let controller = tempfile::tempdir().expect("controller");
    let capability = crate::agents::agent_task_service::test_derived_cook_baseline_capability(
        baseline.path().to_path_buf(),
        "baseline-commit".to_string(),
        "baseline-tree".to_string(),
        "task",
        Some(serde_json::json!({"workspace_snapshot_identity": "snapshot:parent"})),
    );

    assert_eq!(
        super::cook_attempt_source_path(Some(&capability), Some(controller.path())),
        Some(capability.canonical_path())
    );
    assert_eq!(
        capability.verified_baseline_provenance(),
        serde_json::json!({
            "source_run_id": "test-source-run",
            "source_task_id": "task",
            "promoted_patch_artifact_sha256": "test-artifact-sha256",
            "baseline_commit": "baseline-commit",
            "baseline_tree": "baseline-tree",
            "parent_snapshot_identity": "snapshot:parent",
            "preexisting_candidate": false,
        })
    );
}

#[test]
fn lab_cook_attempt_preserves_authorized_dirty_baseline_in_the_run_plan() {
    let mut plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
        "cook-dirty-baseline",
        vec![serde_json::from_value(serde_json::json!({
            "task_id": "task",
            "executor": { "backend": "fixture" },
            "instructions": "continue",
            "workspace": { "root": "/runner/workspace" }
        }))
        .expect("task")],
    );
    let baseline = serde_json::json!({
        "source_run_id": "cook-dirty-baseline",
        "source_task_id": "task",
        "baseline_tree": "0123456789012345678901234567890123456789",
        "preexisting_candidate": true,
    });

    super::attach_verified_cook_baseline(&mut plan, &baseline);
    let serialized = serde_json::to_string(&plan).expect("serialize run plan");
    let run_plan: serde_json::Value = serde_json::from_str(&serialized).expect("parse run plan");

    assert_eq!(
        run_plan["tasks"][0]["metadata"]["verified_cook_baseline"],
        baseline
    );
}

#[test]
fn lab_run_retry_leaves_a_cook_child_for_controller_lifecycle() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path());
        let run_id = "cook-lab-retry-attempt-1";
        let cook_id = "cook-lab-retry";
        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            run_id,
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "cook-lab-retry-task",
                "executor": { "backend": "fixture" },
                "instructions": "Repair the Lab Cook compiler",
                "workspace": { "root": workspace.path() }
            }))
            .expect("task")],
        );
        let options = crate::agents::agent_task_service::CookRequest {
            identity: crate::agents::agent_task_service::CookIdentity {
                cook_id: cook_id.to_string(),
                initial_run_id: run_id.to_string(),
                initial_plan: plan.clone(),
            },
            workspace: crate::agents::agent_task_service::CookWorkspace {
                to_worktree: workspace.path().display().to_string(),
                source_worktree_path: Some(workspace.path().to_path_buf()),
                task_base_sha: None,
                source_refs: Vec::new(),
            },
            provider_transport: crate::agents::agent_task_service::CookProviderTransport {
                provider_command: None,
                provider_invocation: None,
                attempt_dispatcher: None,
            },
            gates: Default::default(),
            retry_policy: crate::agents::agent_task_service::CookRetryPolicy { max_attempts: 2 },
            finalization: crate::agents::agent_task_service::CookFinalization {
                no_finalize: true,
                draft_pr: false,
                base: "main".to_string(),
                head: None,
                title: "Lab Cook retry".to_string(),
                commit_message: "Lab Cook retry".to_string(),
                protected_branches: Vec::new(),
            },
            ai_disclosure: crate::agents::agent_task_service::CookAiDisclosure {
                ai_tool: "fixture".to_string(),
                ai_model: None,
                ai_used_for: "test".to_string(),
            },
            harvest_context:
                homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                    .expect("harvest context"),
        };
        crate::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist Cook recipe");
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("persist Cook attempt");
        agent_task_lifecycle::record_cook_attempt_in_store(
            &homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                .expect("lifecycle store"),
            cook_id,
            1,
            run_id,
        )
        .expect("bind Cook attempt");
        agent_task_lifecycle::record_pre_execution_failure(
            run_id,
            &plan,
            "gate_environment.preserve",
            &Error::validation_invalid_argument("CARGO_HOME", "unavailable", None, None)
                .with_retryable(true),
        )
        .expect("persist retryable Cook failure");
        let args = ["homeboy", "agent-task", "retry", run_id, "--run"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(
            materialize_agent_task_retry_handoff(&Cli::parse_from(&args), &args)
                .expect("inspect Cook retry route")
                .is_none()
        );
        assert!(
            agent_task_lifecycle::cook_index(cook_id)
                .expect("Cook index")
                .attempts
                .iter()
                .all(|attempt| attempt.run_id == run_id),
            "the router must not reserve a standalone retry before Cook resumes it"
        );
    });
}

#[test]
fn preacceptance_io_failure_becomes_a_typed_no_job_receipt() {
    let io_error = std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "Authorization: Bearer route-fixture-secret",
    );
    let error = Error::internal_io(
        io_error.to_string(),
        Some("submit selected Lab runner job".to_string()),
    )
    .with_source(io_error);

    let wrapped =
        durable_lab_preacceptance_transport_error("cook-route-attempt-1", "homeboy-lab", error);
    let receipt: homeboy_lab_contract::lab::transport_failure::LabTransportAttemptReceipt =
        serde_json::from_value(wrapped.details["lab_transport_attempt_receipt"].clone())
            .expect("typed transport receipt");

    assert_eq!(wrapped.code.as_str(), "runner.lab_transport_failure");
    assert_eq!(receipt.selected_runner, "homeboy-lab");
    assert_eq!(
        receipt.acceptance,
        LabJobAcceptanceDisposition::NoJobAccepted
    );
    assert_eq!(
        receipt.error.kind,
        homeboy_lab_contract::lab::transport_failure::LabTransportErrorKind::BrokenPipe
    );
    assert!(!serde_json::to_string(&wrapped.details)
        .expect("serialize wrapped details")
        .contains("route-fixture-secret"));
}

#[test]
fn detached_retry_materializes_failed_plan_and_persists_bounded_preacceptance_failure() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path());
        let source_plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "failed-retry-source",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "retry-task",
                "executor": {
                    "backend": "fixture",
                    "config": { "workspace_root": workspace.path() }
                },
                "instructions": "retry",
                "workspace": { "root": workspace.path() }
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&source_plan, Some("failed-run"))
            .expect("source run submitted");
        let failure = Error::internal_unexpected("provider exited before completion");
        agent_task_lifecycle::record_pre_execution_failure(
            "failed-run",
            &source_plan,
            "provider_execution",
            &failure,
        )
        .expect("source failure persisted");
        let store = homeboy::core::observation::ObservationStore::open_initialized()
            .expect("observation store");
        let mut observed = store
            .get_run("failed-run")
            .expect("read source run")
            .expect("source run exists");
        observed.metadata_json["agent_task_run"]["plan_path"] = serde_json::json!(
            "/home/chubes/.local/share/homeboy/agent-task-runs/failed-run/plan.json"
        );
        store
            .upsert_imported_run_preserving_terminal(&observed)
            .expect("mirror runner transport path");

        let normalized = [
            "homeboy",
            "--detach-after-handoff",
            "--cwd",
            "/controller/homeboy",
            "agent-task",
            "retry",
            "failed-run",
            "--run",
            "--new-run-id",
            "failed-run-retry-on-lab",
            "--runner",
            "homeboy-lab",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let cli = Cli::parse_from([
            "homeboy",
            "--detach-after-handoff",
            "agent-task",
            "retry",
            "failed-run",
            "--run",
            "--new-run-id",
            "failed-run-retry-on-lab",
            "--runner",
            "homeboy-lab",
        ]);
        let handoff = materialize_agent_task_retry_handoff(&cli, &normalized)
            .expect("retry handoff materialized")
            .expect("retry handoff");

        assert!(!handoff.args.iter().any(|arg| arg == "--cwd"));
        assert_eq!(
            handoff.primary_workspace,
            workspace
                .path()
                .canonicalize()
                .expect("canonical workspace")
        );
        assert!(!handoff.args.iter().any(|arg| arg == "/controller/homeboy"));
        let agent_task_index = handoff
            .args
            .iter()
            .position(|arg| arg == "agent-task")
            .expect("agent task");
        assert_eq!(handoff.args[agent_task_index + 1], "run-plan");
        assert_eq!(handoff.args[agent_task_index + 2], "--plan");
        assert_eq!(handoff.args[agent_task_index + 4], "--record-run-id");
        assert_eq!(handoff.args[agent_task_index + 5], handoff.run_id);
        assert_eq!(handoff.run_id, "failed-run-retry-on-lab");
        let remote_cli = Cli::try_parse_from(&handoff.args).expect("portable run-plan argv");
        let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::RunPlan(remote),
        }) = &remote_cli.command
        else {
            panic!("Lab handoff must execute the materialized plan, not discover a retry record");
        };
        assert_eq!(
            remote.record_run_id.as_deref(),
            Some(handoff.run_id.as_str())
        );
        let remote_plan: homeboy::agents::agent_tasks::scheduler::AgentTaskPlan =
            serde_json::from_str(&remote.plan).expect("serialized retry plan");
        assert_eq!(remote_plan, handoff.plan);
        assert_eq!(
            remote_plan.tasks[0].workspace.root.as_deref(),
            Some(workspace.path().to_str().expect("workspace utf8"))
        );
        // The emitted command is accepted by the real CLI parser without
        // inventing a global --cwd. The route carries the selected task
        // checkout separately, and workspace staging maps it to the job cwd.
        assert!(remote_cli.detach_after_handoff);
        let replacement =
            agent_task_lifecycle::reconcile_status(&handoff.run_id).expect("replacement");
        assert_eq!(replacement.metadata["retry_of"], "failed-run");
        assert_eq!(replacement.metadata["retried_from"], "failed-run");
        assert_eq!(replacement.metadata["retry_root"], "failed-run");
        assert_eq!(
            agent_task_lifecycle::reconcile_status("failed-run")
                .expect("source retry lineage")
                .metadata["retries"],
            serde_json::json!(["failed-run-retry-on-lab"])
        );

        // The Lab executes `run-plan` against the durable replacement. Its
        // re-submission must retain retry lineage so later reconciliation can
        // still resolve the retry reservation through the controller store.
        agent_task_lifecycle::submit_plan(&handoff.plan, Some(&handoff.run_id))
            .expect("resubmit replacement from Lab handoff");
        let resubmitted = agent_task_lifecycle::reconcile_status(&handoff.run_id)
            .expect("resubmitted replacement");
        assert_eq!(resubmitted.metadata["retry_of"], "failed-run");
        assert_eq!(resubmitted.metadata["retried_from"], "failed-run");
        assert_eq!(resubmitted.metadata["retry_root"], "failed-run");
        assert_eq!(
            agent_task_lifecycle::reconcile_status("failed-run")
                .expect("source retry lineage remains idempotent")
                .metadata["retries"],
            serde_json::json!(["failed-run-retry-on-lab"])
        );

        stage_retry_lab_handoff_before_preacceptance(Some(&handoff), Some("homeboy-lab"))
            .expect("stage replacement handoff before Lab preacceptance");
        let replacement = agent_task_lifecycle::reconcile_status(&handoff.run_id)
            .expect("staged replacement remains inspectable");
        // Staging binds the controller proxy to its runner. The typed
        // `lab_handoff` is deliberately not written here: since #10855 restored
        // the Lab submission boundary, a pending handoff is only claimed by the
        // submission request itself, so staging cannot pre-announce one.
        assert_eq!(replacement.metadata["kind"], "lab_offload_controller_proxy");
        assert_eq!(replacement.metadata["runner_id"], "homeboy-lab");
        assert!(
            replacement.lab_handoff.is_none(),
            "staging must not claim a handoff the submission has not made"
        );

        let error = persist_retry_handoff_preacceptance_failure(
            &handoff,
            Some("homeboy-lab"),
            Error::internal_unexpected("runner preflight rejected the handoff"),
        );
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("agent-task retry")
                && hint.message.contains(&handoff.run_id)));
        let replacement =
            agent_task_lifecycle::reconcile_status(&handoff.run_id).expect("failed retry");
        assert_eq!(
            replacement.state,
            homeboy::agents::agent_tasks::lifecycle::AgentTaskRunState::Failed
        );
        assert_eq!(
            replacement.metadata["pre_execution_failure"]["phase"],
            "detached_lab_handoff_preacceptance"
        );
    });
}

#[test]
fn controller_owned_run_materializes_plan_for_lab_execution() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "queued-controller-run",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "task",
                "executor": { "backend": "fixture" },
                "instructions": "run this queued task",
                "workspace": { "root": workspace.path() }
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&plan, Some("controller-queued"))
            .expect("submit controller run");
        let args = [
            "homeboy",
            "agent-task",
            "run",
            "controller-queued",
            "--timeout-ms",
            "1200",
            "--runner",
            "homeboy-lab",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let cli = Cli::parse_from(&args);

        let handoff = materialize_agent_task_run_handoff(&cli, &args)
            .expect("materialize run handoff")
            .expect("controller-owned handoff");
        let routed_contract = lab_offload_command_for_materialized_args(&handoff.args)
            .expect("resolve materialized route contract")
            .expect("materialized run-plan remains Lab portable");
        assert_eq!(
            routed_contract.source_path_mode,
            homeboy::core::lab_contract::LabSourcePathMode::CwdOrPathFlag
        );
        assert_eq!(
            routed_contract.workspace_mode_policy,
            homeboy::core::lab_contract::LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot
        );
        let remote_cli = Cli::try_parse_from(&handoff.args).expect("portable run-plan argv");
        let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::RunPlan(remote),
        }) = remote_cli.command
        else {
            panic!("controller run must execute its materialized plan on Lab");
        };

        assert_eq!(remote.record_run_id.as_deref(), Some("controller-queued"));
        assert_eq!(remote.timeout_ms, Some(1200));
        assert_eq!(
            serde_json::from_str::<homeboy::agents::agent_tasks::scheduler::AgentTaskPlan>(
                &remote.plan
            )
            .expect("serialized plan"),
            plan
        );
        assert_eq!(
            handoff.primary_workspace,
            workspace.path().canonicalize().unwrap()
        );
    });
}

#[test]
fn controller_owned_run_refuses_to_handoff_a_plan_without_a_workspace() {
    crate::test_support::with_isolated_home(|_| {
        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "queued-controller-run-without-workspace",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "task",
                "executor": { "backend": "fixture" },
                "instructions": "run this queued task"
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&plan, Some("controller-queued-without-workspace"))
            .expect("submit controller run");
        let args = [
            "homeboy",
            "agent-task",
            "run",
            "controller-queued-without-workspace",
            "--runner",
            "homeboy-lab",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let cli = Cli::parse_from(&args);

        let error = materialize_agent_task_run_handoff(&cli, &args)
            .expect_err("workspace-less controller plan must fail before Lab handoff");

        assert_eq!(error.details["field"], "workspace");
        assert!(error
            .message
            .contains("requires exactly one task workspace"));
    });
}

#[test]
fn unmaterialized_cook_run_stays_controller_local_before_lab_workspace_selection() {
    crate::test_support::with_isolated_home(|_| {
        let run_id = "controller-unmaterialized-cook";
        agent_task_lifecycle::prepare_unmaterialized_cook_admission(
            run_id,
            serde_json::json!({ "request_ref": "sha256:fixture" }),
            "blocked_runner_unavailable",
            "runner disconnected",
        )
        .expect("record unmaterialized admission");
        let args = [
            "homeboy",
            "agent-task",
            "run",
            run_id,
            "--runner",
            "homeboy-lab",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let cli = Cli::parse_from(&args);

        assert!(
            controller_owns_agent_task_lifecycle_command(&cli)
                .expect("resolve controller-owned admission"),
            "the router must not select a Lab workspace for an unmaterialized parent"
        );
        let error = materialize_agent_task_run_handoff(&cli, &args)
            .expect_err("a bypassed route still refuses ordinary run before workspace selection");
        assert!(error.message.contains("fenced resume path"));
        assert!(error
            .hints
            .iter()
            .any(|hint| { hint.message == format!("Run `homeboy agent-task resume {run_id}`.") }));
    });
}

#[test]
fn controller_owned_run_refuses_an_ambiguous_plan_workspace() {
    let first = tempfile::tempdir().expect("first workspace");
    let second = tempfile::tempdir().expect("second workspace");
    let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
        "ambiguous-controller-run",
        vec![
            serde_json::from_value(serde_json::json!({
                "task_id": "first",
                "executor": { "backend": "fixture" },
                "instructions": "first task",
                "workspace": { "root": first.path() }
            }))
            .expect("first task"),
            serde_json::from_value(serde_json::json!({
                "task_id": "second",
                "executor": { "backend": "fixture" },
                "instructions": "second task",
                "workspace": { "root": second.path() }
            }))
            .expect("second task"),
        ],
    );

    let error = plan_primary_workspace(&plan)
        .expect_err("multiple controller plan workspaces must fail before handoff");

    assert_eq!(error.details["field"], "workspace");
    assert!(error.message.contains("multiple task workspaces"));
}

#[test]
fn lab_owned_retry_is_not_read_from_the_controller_store() {
    crate::test_support::with_isolated_home(|_| {
        let args = [
            "homeboy",
            "agent-task",
            "retry",
            "lab-owned-failed-run",
            "--run",
            "--runner",
            "homeboy-lab",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let cli = Cli::parse_from(&args);

        assert!(materialize_agent_task_retry_handoff(&cli, &args)
            .expect("runner-owned retry stays portable")
            .is_none());
    });
}

#[test]
fn explicit_local_cook_does_not_enter_lab_attempt_dispatch() {
    let cli = Cli::parse_from([
        "homeboy",
        "--placement",
        "local",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@local",
        "--verify",
        "true",
        "--prompt",
        "keep this local",
    ]);

    assert!(run_split_placement_cook(
        &cli,
        &[],
        None,
        Some("homeboy-lab"),
        &crate::cli_runtime::placement_directive(&cli, Some("homeboy-lab"), false),
        None,
    )
    .expect("local placement bypasses Lab cook dispatch")
    .is_none());
}

#[test]
fn detached_cook_without_a_lab_runner_does_not_fall_back_to_local_execution() {
    let cli = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@remote",
        "--verify",
        "true",
        "--prompt",
        "queue this remotely",
    ]);

    let error = run_split_placement_cook(
        &cli,
        &[],
        None,
        None,
        &crate::cli_runtime::placement_directive(&cli, None, false),
        None,
    )
    .expect_err("detached Cook must require a remote runner");
    assert!(error
        .message
        .contains("controller-local execution was not authorized"));
}

#[test]
fn lab_cook_defers_provider_destination_and_retry_refuses_unbound_plan() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path());
        // The provider-derived destination is validated against the requested
        // Cook repository through its remote (#11987). The linked worktree
        // created below inherits this remote from its primary.
        git_add_remote(workspace.path(), FIXTURE_REPOSITORY_REMOTE);
        let provider_dir = tempfile::tempdir().expect("provider dir");
        let provider_workspace = provider_dir.path().join("worktree");
        let mut commit_command = Command::new("git");
        commit_command
            .args([
                "-c",
                "user.email=fixture@example.com",
                "-c",
                "user.name=Fixture",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ])
            .current_dir(workspace.path());
        let commit = homeboy::core::test_support::bounded_output(commit_command);
        assert!(commit.status.success(), "{:?}", commit);
        let mut worktree_command = Command::new("git");
        worktree_command
            .args([
                "worktree",
                "add",
                "-b",
                "fix/issue-11291-homeboy",
                provider_workspace
                    .to_str()
                    .expect("utf8 provider workspace"),
            ])
            .current_dir(workspace.path());
        let worktree = homeboy::core::test_support::bounded_output(worktree_command);
        assert!(worktree.status.success(), "{:?}", worktree);
        let provider = provider_dir.path().join("provider");
        let payload = serde_json::json!({
            "worktrees": [{
                "handle": "homeboy@fix-issue-11291-homeboy",
                "path": provider_workspace,
                "branch": "fix/issue-11291-homeboy",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        });
        fs::write(
            &provider,
            format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", payload),
        )
        .expect("write provider");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&provider)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&provider, permissions).expect("make provider executable");
        }
        let mut config = homeboy::core::defaults::HomeboyConfig::default();
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
                    resolve: Some(vec![provider.display().to_string(), "{handle}".to_string()]),
                    ensure: Some(vec![provider.display().to_string(), "{handle}".to_string()]),
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
        homeboy::core::defaults::save_config(&config).expect("save provider config");

        let cook_cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/11291",
            "--verify",
            "true",
            "--backend",
            "fixture",
            "--prompt",
            "retry this task",
        ]);
        let plan = materialize_agent_task_cook_plan(&cook_cli, None)
            .expect("materialize cook plan")
            .expect("cook plan");
        assert_eq!(plan.tasks[0].workspace.root, None);
        assert_eq!(
            plan.tasks[0].metadata["worktree_provision"]["action"],
            "lookup_pending"
        );
        assert_eq!(
            plan.tasks[0].metadata["worktree_provision"]["handle"],
            "homeboy@fix-issue-11291-homeboy"
        );
        agent_task_lifecycle::submit_plan(&plan, Some("failed-run")).expect("submit plan");
        agent_task_lifecycle::record_pre_execution_failure(
            "failed-run",
            &plan,
            "lab_handoff_preacceptance",
            &Error::internal_unexpected("Lab rejected the initial attempt"),
        )
        .expect("persist failed attempt");

        let retry_args = ["homeboy", "agent-task", "retry", "failed-run", "--run"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let retry_cli = Cli::parse_from(&retry_args);
        let error = match materialize_agent_task_retry_handoff(&retry_cli, &retry_args) {
            Err(error) => error,
            Ok(_) => {
                panic!("retry must not substitute the controller cwd for an unresolved workspace")
            }
        };
        assert!(error.message.contains("original persisted plan has none"));
    });
}

#[test]
fn retry_handoff_refuses_multiple_task_workspaces() {
    crate::test_support::with_isolated_home(|_| {
        let first = tempfile::tempdir().expect("first workspace");
        let second = tempfile::tempdir().expect("second workspace");
        git_init(first.path());
        git_init(second.path());
        let task = |id: &str, root: &Path| {
            serde_json::from_value(serde_json::json!({
                "task_id": id,
                "executor": { "backend": "fixture" },
                "instructions": "retry",
                "workspace": { "root": root }
            }))
            .expect("task")
        };
        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "multiple-workspaces",
            vec![task("first", first.path()), task("second", second.path())],
        );
        agent_task_lifecycle::submit_plan(&plan, Some("failed-run")).expect("source plan");
        let source_plan = agent_task_lifecycle::load_plan("failed-run").expect("source plan");
        agent_task_lifecycle::record_pre_execution_failure(
            "failed-run",
            &source_plan,
            "provider_execution",
            &Error::internal_unexpected("failed"),
        )
        .expect("source failure");
        let normalized = ["homeboy", "agent-task", "retry", "failed-run", "--run"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let cli = Cli::parse_from(&normalized);

        let error = match materialize_agent_task_retry_handoff(&cli, &normalized) {
            Ok(_) => panic!("multiple workspaces must fail closed"),
            Err(error) => error,
        };
        assert!(error.message.contains("multiple task workspaces"));
        assert!(agent_task_lifecycle::reconcile_status("failed-run-retry-1").is_err());
    });
}

#[test]
fn retry_handoff_restores_distinct_cook_candidate_source_after_baseline_cleanup() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("candidate source workspace");
        let managed = tempfile::tempdir().expect("managed task workspace");
        let baseline = source.path().join("temporary-baseline");
        git_init(source.path());
        git_init(managed.path());
        std::fs::create_dir(&baseline).expect("create temporary baseline");
        std::fs::remove_dir(&baseline).expect("clean temporary baseline");

        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "cook-distinct-source",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "retry-task",
                "executor": { "backend": "fixture" },
                "instructions": "retry the dirty candidate",
                "workspace": {
                    "root": baseline,
                    "kind": "homeboy-worktree",
                    "materialization": {
                        "kind": "homeboy-worktree",
                        "id": "managed@cook-distinct-source",
                        "root": managed.path(),
                        "branch": "fix/cook-distinct-source",
                    }
                },
                "metadata": {
                    "cook_continuation_workspace": {
                        "candidate_source_root": source.path(),
                        "task_workspace": {
                            "root": managed.path(),
                            "kind": "homeboy-worktree",
                            "materialization": {
                                "kind": "homeboy-worktree",
                                "id": "managed@cook-distinct-source",
                                "root": managed.path(),
                                "branch": "fix/cook-distinct-source",
                            }
                        }
                    },
                    "cook_initial_candidate_baseline": {
                        "source_root": source.path(),
                        "commit": "fixture-commit",
                        "tree": "fixture-tree",
                    }
                }
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&plan, Some("failed-run")).expect("source plan");
        agent_task_lifecycle::record_pre_execution_failure(
            "failed-run",
            &plan,
            "controller_admission",
            &Error::internal_unexpected("Lab rejected the initial attempt"),
        )
        .expect("source failure");

        let normalized = ["homeboy", "agent-task", "retry", "failed-run", "--run"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let cli = Cli::parse_from(&normalized);
        let handoff = materialize_agent_task_retry_handoff(&cli, &normalized)
            .expect("retry handoff materialized")
            .expect("retry handoff");

        assert_eq!(
            handoff.primary_workspace,
            source.path().canonicalize().expect("candidate source root")
        );
        assert_eq!(
            handoff.plan.tasks[0].workspace.root.as_deref(),
            Some(source.path().to_str().expect("source path"))
        );
        assert_eq!(
            handoff.plan.tasks[0].metadata["cook_continuation_workspace"]["task_workspace"]["root"],
            serde_json::json!(managed.path())
        );
    });
}

#[test]
fn retry_prefers_managed_worktree_over_cleaned_up_ephemeral_baseline() {
    crate::test_support::with_isolated_home(|_| {
        // The authoritative managed worktree the cook was anchored to. It
        // outlives the attempt and stays a real git checkout.
        let managed = tempfile::tempdir().expect("managed worktree");
        git_init(managed.path());
        homeboy::core::worktree::adopt(homeboy::core::worktree::WorktreeAdoptOptions {
            handle: "fixture@cook".to_string(),
            path: managed.path().display().to_string(),
            kind: None,
            provenance: None,
        })
        .expect("adopt managed worktree");

        // The ephemeral initial-baseline directory the original plan recorded
        // as workspace.root, then cleaned up before retry.
        let ephemeral = tempfile::tempdir().expect("ephemeral baseline");
        let ephemeral_root = ephemeral.path().to_path_buf();
        git_init(&ephemeral_root);
        drop(ephemeral); // simulate baseline cleanup: the path no longer exists.
        assert!(!ephemeral_root.exists());

        // The persisted task still carries both the dead ephemeral root and the
        // durable managed worktree handle (its slug).
        let task: homeboy::agents::agent_tasks::AgentTaskRequest =
            serde_json::from_value(serde_json::json!({
                "task_id": "cook-task",
                "executor": { "backend": "fixture" },
                "instructions": "retry",
                "workspace": { "root": ephemeral_root, "slug": "fixture@cook" }
            }))
            .expect("task");
        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "cleaned-up-baseline",
            vec![task],
        );

        let resolved = retry_plan_primary_workspace(&plan)
            .expect("retry resolves the managed worktree despite the cleaned-up baseline");
        let expected = homeboy::core::git::repo_root(managed.path()).expect("managed git root");
        assert_eq!(
            resolved, expected,
            "retry must continue against the managed worktree, never the deleted ephemeral baseline"
        );
    });
}

#[test]
fn retry_reports_recoverable_state_when_the_managed_worktree_is_gone() {
    crate::test_support::with_isolated_home(|_| {
        // A managed worktree was recorded, then its checkout removed from disk
        // while the record still points at it.
        let managed = tempfile::tempdir().expect("managed worktree");
        let managed_path = managed.path().to_path_buf();
        git_init(&managed_path);
        homeboy::core::worktree::adopt(homeboy::core::worktree::WorktreeAdoptOptions {
            handle: "fixture@gone".to_string(),
            path: managed_path.display().to_string(),
            kind: None,
            provenance: None,
        })
        .expect("adopt managed worktree");
        drop(managed);
        assert!(!managed_path.exists());

        let task: homeboy::agents::agent_tasks::AgentTaskRequest =
            serde_json::from_value(serde_json::json!({
                "task_id": "cook-task",
                "executor": { "backend": "fixture" },
                "instructions": "retry",
                "workspace": { "slug": "fixture@gone" }
            }))
            .expect("task");
        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "missing-worktree",
            vec![task],
        );

        let error = retry_plan_primary_workspace(&plan)
            .expect_err("a missing managed worktree must return a precise recoverable state");
        assert!(
            error.message.contains("points at a missing checkout"),
            "unexpected error: {}",
            error.message
        );
    });
}

#[test]
fn retry_handoff_identifies_an_original_plan_without_a_workspace() {
    crate::test_support::with_isolated_home(|_| {
        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "missing-workspace",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "retry-task",
                "executor": { "backend": "fixture" },
                "instructions": "retry"
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&plan, Some("failed-run")).expect("source plan");
        let source_plan = agent_task_lifecycle::load_plan("failed-run").expect("source plan");
        agent_task_lifecycle::record_pre_execution_failure(
            "failed-run",
            &source_plan,
            "provider_execution",
            &Error::internal_unexpected("failed"),
        )
        .expect("source failure");
        let normalized = ["homeboy", "agent-task", "retry", "failed-run", "--run"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let cli = Cli::parse_from(&normalized);

        let error = match materialize_agent_task_retry_handoff(&cli, &normalized) {
            Ok(_) => panic!("missing original workspace must fail before creating a retry"),
            Err(error) => error,
        };

        assert!(error.message.contains("original persisted plan has none"));
        assert!(agent_task_lifecycle::reconcile_status("failed-run-retry-1").is_err());
    });
}

#[test]
fn agent_task_fanout_finish_metadata_preserves_discoverability_commands() {
    let metadata = agent_task_fanout_finish_metadata(
        serde_json::json!({
            "lab_dispatch": {
                "status": "error",
                "runner_id": "homeboy-lab",
            },
        }),
        "dispatch-run-7167",
        "cook-batch-homeboy-issue-7167-1",
        RunStatus::Error,
    );

    assert_eq!(
        metadata["agent_task_lab_dispatch"]["fanout_id"],
        "cook-batch-homeboy-issue-7167-1"
    );
    assert_eq!(metadata["agent_task_lab_dispatch"]["status"], "error");
    assert_eq!(
        metadata["follow_commands"]["dispatch_status"],
        "homeboy runs show dispatch-run-7167"
    );
    assert_eq!(
        metadata["follow_commands"]["dispatch_evidence"],
        "homeboy runs evidence --run dispatch-run-7167"
    );
    assert_eq!(
        metadata["follow_commands"]["fanout_status"],
        "homeboy agent-task fanout status cook-batch-homeboy-issue-7167-1"
    );
}

#[test]
fn offloaded_stdout_write_preserves_bytes_for_output_file() {
    let dir = tempdir().unwrap();
    let output_path = dir.path().join("out.json");

    write_offloaded_stdout(&output_path.to_string_lossy(), "{\"ok\":true}\n").unwrap();

    assert_eq!(
        std::fs::read_to_string(output_path).unwrap(),
        "{\"ok\":true}\n"
    );
}

#[test]
fn runner_rig_source_management_remote_preflight_strips_controller_globals() {
    let normalized = vec![
        "homeboy".to_string(),
        "rig".to_string(),
        "sources".to_string(),
        "list".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "--output=./sources.json".to_string(),
        "--placement".to_string(),
        "lab-or-local".to_string(),
        "--placement=lab".to_string(),
        "--detach-after-handoff".to_string(),
    ];

    let command = runner_rig_source_management_command("/usr/local/bin/homeboy", &normalized);
    let preflight = strip_rig_source_management_local_wrapper_flags(&command);

    assert_eq!(
        preflight,
        vec![
            "/usr/local/bin/homeboy".to_string(),
            "rig".to_string(),
            "sources".to_string(),
            "list".to_string(),
        ]
    );
}

#[test]
fn runner_rig_source_management_translates_local_subdir_paths() {
    let command = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        "/Users/chubes/Developer/homeboy-rigs@run/WordPress/static-site-importer".to_string(),
    ];

    let translated = translate_command_path_prefix(
        &command,
        std::path::Path::new("/Users/chubes/Developer/homeboy-rigs@run"),
        "/home/chubes/Developer/_lab_workspaces/homeboy-rigs-run-abc",
    );

    assert_eq!(
        translated[3],
        "/home/chubes/Developer/_lab_workspaces/homeboy-rigs-run-abc/WordPress/static-site-importer"
    );
}

#[test]
fn rig_install_source_arg_finds_positional_source_after_flags() {
    let command = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        "--id".to_string(),
        "static-site-importer".to_string(),
        "--reinstall".to_string(),
        "/Users/chubes/Developer/homeboy-rigs@run/WordPress/static-site-importer".to_string(),
        "--all".to_string(),
    ];

    assert_eq!(
        rig_install_source_arg(&command).as_deref(),
        Some("/Users/chubes/Developer/homeboy-rigs@run/WordPress/static-site-importer")
    );
}

#[test]
fn rig_install_source_arg_ignores_non_install_commands() {
    let command = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "sources".to_string(),
        "list".to_string(),
    ];

    assert_eq!(rig_install_source_arg(&command), None);
}

#[test]
fn rig_install_source_sync_root_resolves_existing_local_package() {
    let source_dir = tempdir().expect("source dir");
    let source_path = source_dir
        .path()
        .canonicalize()
        .expect("canonical temp dir")
        .join("static-site-importer");
    fs::create_dir_all(&source_path).expect("create source package");
    let command = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        source_path.to_string_lossy().to_string(),
    ];

    let sync_root = rig_install_source_sync_root(&command).expect("sync root");

    // The temp dir is not a git repo, so the package directory itself is the
    // materialization root.
    assert_eq!(sync_root, source_path);
}

#[test]
fn rig_install_source_sync_root_skips_git_url_and_missing_paths() {
    let git_url = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        "https://github.com/Extra-Chill/homeboy-rigs.git".to_string(),
    ];
    assert_eq!(rig_install_source_sync_root(&git_url), None);

    let missing = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        "/Users/chubes/Developer/does-not-exist-rig-package-6964".to_string(),
    ];
    assert_eq!(rig_install_source_sync_root(&missing), None);
}

#[test]
fn local_fanout_warning_covers_batch_fanout_not_just_cook() {
    // Batch fanout is the command most able to overwhelm a controller, so a
    // silent local fallback here is worse than for a single cook.
    let cli = Cli::parse_from([
        "homeboy",
        "agent-task",
        "fanout",
        "cook-batch",
        "https://github.com/o/r/issues/1",
        "https://github.com/o/r/issues/2",
        "--repo",
        "r",
        "--verify",
        "cargo test",
        "--run-plan",
    ]);
    let warning =
        agent_task_local_fanout_warning(&cli.command, None).expect("batch fanout warns locally");
    assert!(warning.contains("HOMEBOY_LOCAL_FANOUT_WARNING"));
    assert!(warning.contains("fanout cook-batch"));
    assert!(warning.contains("tasks=2"));
    assert!(warning.contains("execution_location=local"));
}

#[test]
fn local_fanout_warning_is_silent_for_a_single_child() {
    let cli = Cli::parse_from([
        "homeboy",
        "agent-task",
        "fanout",
        "cook-batch",
        "https://github.com/o/r/issues/1",
        "--repo",
        "r",
        "--verify",
        "cargo test",
        "--run-plan",
    ]);
    assert_eq!(agent_task_local_fanout_warning(&cli.command, None), None);
}

#[test]
fn local_fanout_warning_is_silent_for_a_dry_run_plan() {
    // A dry run never dispatches providers, so it cannot heat the controller.
    let cli = Cli::parse_from([
        "homeboy",
        "agent-task",
        "fanout",
        "cook-batch",
        "https://github.com/o/r/issues/1",
        "https://github.com/o/r/issues/2",
        "--repo",
        "r",
        "--verify",
        "cargo test",
        "--dry-run",
        "--run-plan",
    ]);
    assert_eq!(agent_task_local_fanout_warning(&cli.command, None), None);
}

#[test]
fn local_fanout_warning_carries_lab_readiness_reasons_and_remediation() {
    let cli = Cli::parse_from([
        "homeboy",
        "agent-task",
        "fanout",
        "cook-batch",
        "https://github.com/o/r/issues/1",
        "https://github.com/o/r/issues/2",
        "--repo",
        "r",
        "--verify",
        "cargo test",
        "--run-plan",
    ]);
    let readiness = homeboy::core::parsed_command_preflight::LabReadinessSnapshot {
        state: "stale".to_string(),
        selected_runner_id: None,
        available_runner_ids: vec!["lab-a".to_string()],
        reasons: vec!["lab-a daemon is stale".to_string()],
        remediation_commands: vec!["homeboy runner refresh-homeboy lab-a".to_string()],
    };
    let warning = agent_task_local_fanout_warning(&cli.command, Some(&readiness))
        .expect("batch fanout warns locally");
    // Without these the operator has no way to learn why the Lab was skipped.
    assert!(warning.contains("lab_unavailable_reason=lab-a daemon is stale"));
    assert!(warning.contains("remediation=`homeboy runner refresh-homeboy lab-a`"));
}

#[test]
fn batch_fanout_exposes_placement_arguments() {
    // Split placement hands each child to the selected runner, so hiding these
    // made a load-bearing flag undiscoverable.
    for path in [
        ["agent-task", "fanout", "cook-batch"],
        ["agent-task", "fanout", "run-plan"],
    ] {
        let command = homeboy::cli_surface::Cli::command_with_scoped_lab_args();
        let leaf = path.iter().fold(&command, |command, segment| {
            command
                .get_subcommands()
                .find(|candidate| candidate.get_name() == *segment)
                .unwrap_or_else(|| panic!("{segment} subcommand exists"))
        });
        for flag in ["placement", "runner"] {
            let arg = leaf
                .get_arguments()
                .find(|arg| arg.get_id() == flag)
                .unwrap_or_else(|| panic!("{} exposes --{flag}", path.join(" ")));
            assert!(
                !arg.is_hide_set(),
                "{} must advertise --{flag}",
                path.join(" ")
            );
        }
    }
}

/// #9373: docs recommend global `--placement lab` for Cook waves and the
/// runtime honors it, so a cook that CAN be served must never be reported as a
/// command for which Lab placement is unavailable.
#[test]
fn split_placement_cook_accepts_lab_placement_when_a_runner_is_selected() {
    let cli = Cli::parse_from([
        "homeboy",
        "--placement",
        "lab",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@wave",
        "--verify",
        "true",
        "--prompt",
        "route this attempt to Lab",
    ]);

    assert!(
        split_placement_lab_runner_unavailable_error(
            &cli.command,
            cli.placement,
            Some("homeboy-lab"),
            None,
        )
        .is_none(),
        "a selected runner serves --placement lab instead of refusing it"
    );
}

#[test]
fn detached_cook_is_the_only_split_placement_command_eligible_for_queue_admission() {
    let queued = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@queue",
        "--verify",
        "true",
        "--prompt",
        "queue this on Lab",
    ]);
    assert!(detached_cook_can_queue(&queued));

    let local = Cli::parse_from([
        "homeboy",
        "--placement",
        "local",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@local",
        "--verify",
        "true",
        "--prompt",
        "remain local",
    ]);
    assert!(!detached_cook_can_queue(&local));
}

#[test]
fn unmaterialized_cook_admission_keeps_stale_unavailable_and_capacity_distinct() {
    let readiness = |state: &str| homeboy::core::parsed_command_preflight::LabReadinessSnapshot {
        state: state.to_string(),
        selected_runner_id: None,
        available_runner_ids: Vec::new(),
        reasons: Vec::new(),
        remediation_commands: Vec::new(),
    };
    assert_eq!(
        unmaterialized_admission_state(Some(&readiness("stale"))),
        "blocked_runner_stale"
    );
    assert_eq!(
        unmaterialized_admission_state(Some(&readiness("disconnected"))),
        "blocked_runner_unavailable"
    );
    assert_eq!(
        unmaterialized_admission_state(Some(&readiness("capacity_blocked"))),
        "queued"
    );
}

#[test]
fn admission_capability_contract_accepts_runtime_or_capability_and_rejects_missing() {
    let inventory = runners::RunnerCapabilityInventory {
        runtime_ids: ["runtime-a".to_string()].into_iter().collect(),
        capabilities: ["capability-a".to_string()].into_iter().collect(),
    };
    assert!(
        crate::cli_runtime::runner_inventory_satisfies_admission_capabilities(
            &inventory,
            &["runtime-a", "capability-a"].into_iter().collect(),
        )
    );
    assert!(
        !crate::cli_runtime::runner_inventory_satisfies_admission_capabilities(
            &inventory,
            &["missing"].into_iter().collect(),
        )
    );
}

#[test]
fn replay_intent_reconstructs_cook_from_references_without_secret_values() {
    homeboy::core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("inputs");
        let provider = temp.path().join("provider.json");
        std::fs::write(
            &provider,
            r#"{"secret_env":["PROVIDER_TOKEN"],"token_env":"PROVIDER_TOKEN","credential_ref":"configured/provider"}"#,
        )
        .expect("provider config");
        let args = vec![
            "homeboy".to_string(),
            "--detach-after-handoff".to_string(),
            "--placement".to_string(),
            "auto".to_string(),
            "--runner-secret-env".to_string(),
            "API_TOKEN".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--run-id".to_string(),
            "replay-intent-cook".to_string(),
            "--to-worktree".to_string(),
            "repo@fix-12443".to_string(),
            "--prompt".to_string(),
            "implement the requested change".to_string(),
            "--private-verify".to_string(),
            "test -n \"$API_TOKEN\"".to_string(),
            "--provider-config".to_string(),
            format!("@{}", provider.display()),
        ];
        let intent = build_unmaterialized_cook_replay_intent(&args, "replay-intent-cook", None)
            .expect("replay intent");
        let serialized = serde_json::to_string(&intent).expect("serialize intent");
        assert!(!serialized.contains("PROVIDER_TOKEN"), "{serialized}");
        assert!(
            !serialized.contains("implement the requested change"),
            "{serialized}"
        );
        assert!(!serialized.contains("test -n"), "{serialized}");
        assert!(
            serialized.contains("API_TOKEN"),
            "secret name is a reference"
        );
        assert!(intent
            .argv
            .iter()
            .any(|arg| arg == "--detach-after-handoff"));
        assert!(intent
            .argv
            .windows(2)
            .any(|pair| pair[0] == "--placement" && pair[1] == "auto"));
        assert!(intent.argv.iter().any(|arg| arg == "--prompt"));
        assert!(intent.argv.iter().any(|arg| arg == "--private-verify-file"));
        assert_eq!(intent.input_refs.len(), 3);
        assert!(intent
            .input_refs
            .iter()
            .all(|reference| !reference.path.starts_with(temp.path().to_str().unwrap())));
        let replayed_intent =
            build_unmaterialized_cook_replay_intent(&args, "replay-intent-cook", None)
                .expect("idempotent replay intent");
        assert_eq!(replayed_intent, intent);
        assert!(intent
            .argv
            .windows(2)
            .any(|pair| pair[0] == "--prompt" && pair[1].starts_with("homeboy-replay-ref:")));
        let mut replay = intent.argv.clone();
        replay.extend(["--runner".to_string(), "lab".to_string()]);
        let replay =
            Cli::try_parse_from(replay).expect("intent reconstructs normal Cook CLI inputs");
        assert_eq!(replay.runner.as_deref(), Some("lab"));
        assert!(
            replay.runner.is_some() && !replay.placement.is_explicit_local_override(),
            "replay pins Lab without authorizing local placement"
        );
    });
}

#[test]
fn replay_intent_preserves_explicit_lab_placement_without_a_runner_pin() {
    homeboy::core::test_support::with_isolated_home(|_| {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--placement".to_string(),
            "lab".to_string(),
            "--run-id".to_string(),
            "explicit-lab-replay".to_string(),
            "--to-worktree".to_string(),
            "repo@explicit-lab".to_string(),
            "--prompt".to_string(),
            "preserve the explicit placement".to_string(),
        ];
        let intent = build_unmaterialized_cook_replay_intent(&args, "explicit-lab-replay", None)
            .expect("replay intent");
        let replay = Cli::try_parse_from(&intent.argv).expect("intent parses as Cook");

        assert_eq!(replay.placement, crate::cli_surface::Placement::Lab);
        assert_eq!(replay.runner, None);
    });
}

#[test]
fn replay_intent_snapshots_all_arbitrary_text_and_rejects_credential_urls() {
    homeboy::core::test_support::with_isolated_home(|_| {
        let content = [
            ("--task", "task body"),
            ("--goal", "goal body"),
            ("--title", "title body"),
            ("--commit-message", "commit body"),
            ("--command-policy-reason", "policy body"),
        ];
        let mut args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--run-id".to_string(),
            "text-replay".to_string(),
        ];
        for (flag, value) in content {
            args.extend([flag.to_string(), value.to_string()]);
        }
        let intent = build_unmaterialized_cook_replay_intent(&args, "text-replay", None)
            .expect("text-backed intent");
        let serialized = serde_json::to_string(&intent).unwrap();
        for (_, value) in content {
            assert!(!serialized.contains(value), "leaked {value}: {serialized}");
        }
        assert_eq!(
            intent
                .input_refs
                .iter()
                .filter(|reference| reference.argv_token.is_some())
                .count(),
            content.len()
        );

        let unsafe_url = [
            "homeboy",
            "agent-task",
            "cook",
            "--run-id",
            "url-replay",
            "--task-url",
            "https://example.test/task?token=secret",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let error = build_unmaterialized_cook_replay_intent(&unsafe_url, "url-replay", None)
            .expect_err("credential URL rejected");
        assert!(error.message.contains("unsafe inline value"), "{error:?}");
    });
}

#[test]
fn replay_input_failure_never_commits_a_manifest_and_retry_is_clean() {
    homeboy::core::test_support::with_isolated_home(|_| {
        let cook_id = "missing-replay-input";
        let temp = tempfile::tempdir().unwrap();
        let provider = temp.path().join("provider.json");
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--run-id".to_string(),
            cook_id.to_string(),
            "--prompt".to_string(),
            "durable prompt".to_string(),
            "--provider-config".to_string(),
            format!("@{}", provider.display()),
        ];
        build_unmaterialized_cook_replay_intent(&args, cook_id, None)
            .expect_err("missing input fails intent");
        let root = replay_intent_storage_root(cook_id).expect("root");
        assert!(!root.exists(), "failed snapshot must not publish inputs");
        let parent = root
            .parent()
            .and_then(std::path::Path::parent)
            .expect("admissions root");
        assert!(
            std::fs::read_dir(parent)
                .expect("read admissions root")
                .all(|entry| !entry
                    .expect("admission entry")
                    .file_name()
                    .to_string_lossy()
                    .contains("-inputs-stage-")),
            "failed snapshot staging bytes must be cleaned"
        );
        let manifests = std::fs::read_dir(root)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains("manifest"))
            .count();
        assert_eq!(manifests, 0);
        std::fs::write(&provider, "{}").unwrap();
        let intent = build_unmaterialized_cook_replay_intent(&args, cook_id, None)
            .expect("retry completes the same immutable input set");
        assert!(std::path::Path::new(&intent.input_manifest.path).is_file());
        assert_eq!(intent.input_refs.len(), 2);
    });
}

#[test]
fn replay_worker_supervisor_reaps_and_releases_an_unpublished_claim() {
    homeboy::core::test_support::with_isolated_home(|_| {
        let cook_id = "supervised-replay-worker";
        agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
            &homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                .expect("lifecycle store"),
            cook_id,
            serde_json::json!({ "request_ref": "sha256:request" }),
            "queued",
            "eligible",
        )
        .expect("admitted");
        agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
            record.metadata["unmaterialized_cook_admission"]["fence"] = serde_json::json!(1);
            record.metadata["unmaterialized_cook_admission"]["lease"] = serde_json::json!({
                "state": "claimed",
                "fence": 1,
                "token": "supervised-token",
                "expires_at": "2999-01-01T00:00:00+00:00",
            });
        })
        .expect("claimed");
        let child = std::process::Command::new("sh")
            .args(["-c", "exit 17"])
            .spawn()
            .expect("spawn deterministic replay worker");
        supervise_replay_worker(
            cook_id.to_string(),
            1,
            "supervised-token".to_string(),
            child,
        )
        .join()
        .expect("supervisor joins");
        let record = agent_task_lifecycle::exact_record(cook_id).expect("released claim");
        assert_eq!(
            record.metadata["unmaterialized_cook_admission"]["lease"]["state"],
            "released"
        );
    });
}

#[test]
fn rejected_admission_rebinding_writes_no_snapshot_bytes() {
    homeboy::core::test_support::with_isolated_home(|_| {
        let cook_id = "snapshot-rebinding";
        agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
            &homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                .expect("lifecycle store"),
            cook_id,
            serde_json::json!({ "request_ref": "sha256:first" }),
            "queued",
            "eligible",
        )
        .expect("first binding");
        let root = replay_intent_storage_root(cook_id).expect("snapshot root");
        assert!(!root.exists());

        let error = agent_task_lifecycle::precheck_unmaterialized_cook_admission(
            cook_id,
            "sha256:rejected",
        )
        .expect_err("rebound request rejected before staging");
        assert!(error.message.contains("different unmaterialized admission"));
        assert!(!root.exists(), "rejected request wrote snapshot bytes");
        let parent = root.parent().expect("snapshot parent");
        assert!(
            !parent.exists()
                || std::fs::read_dir(parent)
                    .expect("read snapshot parent")
                    .next()
                    .is_none(),
            "rejected request left bytes in the admission directory"
        );
    });
}

#[test]
fn daemon_recovers_preparing_snapshot_after_crash_before_publish() {
    homeboy::core::test_support::with_isolated_home(|_| {
        let cook_id = "snapshot-publication-retry";
        let args = [
            "homeboy",
            "agent-task",
            "cook",
            "--run-id",
            cook_id,
            "--prompt",
            "retryable snapshot",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let staged = stage_unmaterialized_cook_replay_intent(&args, cook_id, None)
            .expect("first staged snapshot");
        let intent = staged.intent.as_ref().expect("staged intent").clone();
        let binding = serde_json::json!({
            "request_ref": "sha256:stable-request",
            "replay_intent": intent,
            "input_publication": {
                "state": "staged",
                "staging_root": staged.staging_root.display().to_string(),
                "published_root": staged.published_root.display().to_string(),
            },
        });
        let prepared = agent_task_lifecycle::prepare_unmaterialized_cook_admission(
            cook_id, binding, "queued", "eligible",
        )
        .expect("accepted binding");
        assert_eq!(
            prepared.metadata["unmaterialized_cook_admission"]["state"],
            "preparing_inputs"
        );
        assert_eq!(
            prepared.metadata["detached_cook_handoff"]["cook_id"],
            cook_id
        );
        staged.retain_for_recovery();
        let root = replay_intent_storage_root(cook_id).expect("published root");
        assert!(!root.exists(), "simulated crash precedes atomic rename");
        crate::agents::agent_task_service::reconcile_unmaterialized_cook_admission_with(
            cook_id,
            |_| panic!("freshly published admission is not due for selection"),
            |_| panic!("freshly published admission is not replayed"),
        )
        .expect("daemon recovers durable staging");
        let recovered = agent_task_lifecycle::exact_record(cook_id).expect("recovered admission");
        assert_eq!(
            recovered.metadata["unmaterialized_cook_admission"]["state"],
            "queued"
        );
        assert!(root.is_dir());
    });
}

#[test]
fn daemon_recovers_preparing_state_after_crash_after_publish() {
    homeboy::core::test_support::with_isolated_home(|_| {
        let cook_id = "snapshot-published-before-state";
        let args = ["homeboy", "agent-task", "cook", "--run-id", cook_id]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let staged =
            stage_unmaterialized_cook_replay_intent(&args, cook_id, None).expect("staged snapshot");
        let binding = serde_json::json!({
            "request_ref": "sha256:published-request",
            "replay_intent": staged.intent.as_ref().expect("intent"),
            "input_publication": {
                "state": "staged",
                "staging_root": staged.staging_root.display().to_string(),
                "published_root": staged.published_root.display().to_string(),
            },
        });
        agent_task_lifecycle::prepare_unmaterialized_cook_admission(
            cook_id,
            binding,
            "blocked_runner_unavailable",
            "waiting",
        )
        .expect("prepared");
        staged.publish().expect("publish before simulated crash");

        crate::agents::agent_task_service::reconcile_unmaterialized_cook_admission_with(
            cook_id,
            |_| panic!("freshly published admission is not due for selection"),
            |_| panic!("freshly published admission is not replayed"),
        )
        .expect("daemon converges published snapshot");
        let recovered = agent_task_lifecycle::exact_record(cook_id).expect("recovered admission");
        assert_eq!(
            recovered.metadata["unmaterialized_cook_admission"]["state"],
            "blocked_runner_unavailable"
        );
        assert_eq!(
            recovered.metadata["unmaterialized_cook_admission"]["binding"]["input_publication"]
                ["state"],
            "published"
        );
    });
}

#[test]
fn replay_intent_rejects_inline_secret_bearing_inputs() {
    homeboy::core::test_support::with_isolated_home(|_| {
        for unsafe_args in [
            vec!["--runner-env", "TOKEN=value"],
            vec!["--gate-env", "TOKEN=value"],
            vec!["--lab-env-json", r#"{"TOKEN":"value"}"#],
            vec!["--provider-config", r#"{"token":"value"}"#],
            vec!["--provider-argv", "token=value"],
            vec!["--provider-evidence", r#"{"password":"value"}"#],
        ] {
            let args = [
                vec!["homeboy", "agent-task", "cook", "--run-id", "unsafe-replay"],
                unsafe_args,
            ]
            .concat()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
            let error = build_unmaterialized_cook_replay_intent(&args, "unsafe-replay", None)
                .expect_err("unsafe inline input rejected");
            assert!(error.message.contains("unsafe inline value"), "{error:?}");
        }

        let temp = tempfile::tempdir().expect("provider input");
        for (name, content) in [
            ("provider.json", r#"{"token":"plaintext-provider-token"}"#),
            ("client.json", r#"{"authorization":"Bearer plaintext"}"#),
        ] {
            let path = temp.path().join(name);
            std::fs::write(&path, content).expect("secret fixture");
            let flag = if name == "provider.json" {
                "--provider-config"
            } else {
                "--client-context"
            };
            let args = [
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--run-id".to_string(),
                format!("unsafe-{name}"),
                flag.to_string(),
                format!("@{}", path.display()),
            ];
            let error =
                build_unmaterialized_cook_replay_intent(&args, &format!("unsafe-{name}"), None)
                    .expect_err("secret-bearing snapshot rejected");
            assert!(error.message.contains("unsafe inline value"), "{error:?}");
        }
    });
}

/// The failure an operator actually hits is "no ready Lab runner", not
/// "`--placement lab` is unavailable for this local-only command". Reporting the
/// portability contract there contradicts the documented wave guidance (#9373).
#[test]
fn split_placement_cook_without_a_runner_reports_readiness_not_a_placement_contradiction() {
    let cli = Cli::parse_from([
        "homeboy",
        "--placement",
        "lab",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@wave",
        "--verify",
        "true",
        "--prompt",
        "route this attempt to Lab",
    ]);
    for (state, reason, remediation) in [
        (
            "stale",
            "homeboy-lab daemon is stale",
            "homeboy runner doctor homeboy-lab --scope lab-offload",
        ),
        (
            "disconnected",
            "homeboy-lab is disconnected",
            "homeboy runner connect homeboy-lab",
        ),
        (
            "capacity_blocked",
            "homeboy-lab reached capacity",
            "homeboy runner status homeboy-lab",
        ),
    ] {
        let readiness = homeboy::core::parsed_command_preflight::LabReadinessSnapshot {
            state: state.to_string(),
            selected_runner_id: None,
            available_runner_ids: Vec::new(),
            reasons: vec![reason.to_string()],
            remediation_commands: vec![remediation.to_string()],
        };

        let error = split_placement_lab_runner_unavailable_error(
            &cli.command,
            cli.placement,
            None,
            Some(&readiness),
        )
        .expect("lab placement with no runner must be explained");

        assert_eq!(error.details["field"].as_str(), Some("placement"));
        let problem = error.details["problem"].as_str().expect("problem");
        assert!(
            problem.contains("accepts `--placement lab`"),
            "guidance and runtime must agree: {problem}"
        );
        assert!(
            problem.contains(state),
            "the readiness verdict is the real cause: {problem}"
        );
        assert!(
            problem.contains("controller-owned target preparation did not start"),
            "the refusal must name the controller phase: {problem}"
        );
        let hints = error.details["tried"]
            .as_array()
            .expect("remediation hints")
            .iter()
            .filter_map(|hint| hint.as_str())
            .collect::<Vec<_>>();
        assert!(
            hints.iter().any(|hint| hint.contains(remediation)),
            "readiness remediation must be carried through: {hints:?}"
        );
        assert!(
            hints
                .iter()
                .any(|hint| hint.contains("`--placement lab` is the supported spelling")),
            "remediation must confirm the documented spelling: {hints:?}"
        );
    }
}

#[test]
fn lab_cook_preparation_failure_names_controller_phase_and_selected_runner() {
    let error = Error::validation_invalid_argument(
        "to_worktree",
        "worktree provider lookup failed",
        Some("fixture@target".to_string()),
        None,
    );

    let error = annotate_cook_controller_preparation_error(error, "homeboy-lab");

    assert!(error
        .message
        .contains("controller-owned target preparation"));
    assert!(error
        .message
        .contains("before its Lab provider attempt was dispatched"));
    assert_eq!(error.details["cook_phase"], "controller_target_preparation");
    assert_eq!(error.details["provider_execution_placement"], "lab");
    assert_eq!(error.details["selected_runner_id"], "homeboy-lab");
    assert!(error.hints.iter().any(|hint| hint
        .message
        .contains("repair this controller preparation failure")));
}

/// `--placement lab-or-local` authorizes controller execution, so it must keep
/// falling back instead of failing when no runner is ready.
#[test]
fn lab_or_local_placement_still_falls_back_without_a_runner() {
    let cli = Cli::parse_from([
        "homeboy",
        "--placement",
        "lab-or-local",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@wave",
        "--verify",
        "true",
        "--prompt",
        "fall back locally",
    ]);

    assert!(
        split_placement_lab_runner_unavailable_error(&cli.command, cli.placement, None, None)
            .is_none()
    );
}

#[test]
fn cold_lab_or_local_records_an_admitted_local_fallback() {
    let source = tempfile::tempdir().expect("source workspace");
    let cli = Cli::parse_from([
        "homeboy",
        "--placement",
        "lab-or-local",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@wave",
        "--verify",
        "true",
        "--prompt",
        "fall back locally when admitted",
    ]);

    let decision =
        fixture_preflight_decision(&cli, None, "cook-provider-attempt", Some(source.path()))
            .expect("cold controller may select local fallback");

    assert!(decision.permits_local_execution());
    assert!(decision.fallback.local_allowed);
    assert!(!decision.override_authorization.authorized);
}

#[test]
fn explicit_local_placement_records_audited_override_authorization() {
    let source = tempfile::tempdir().expect("source workspace");
    let cli = Cli::parse_from([
        "homeboy",
        "--placement",
        "local",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@wave",
        "--verify",
        "true",
        "--prompt",
        "run locally with operator authorization",
    ]);

    let decision =
        fixture_preflight_decision(&cli, None, "cook-provider-attempt", Some(source.path()))
            .expect("explicit local placement resolves");

    assert!(decision.permits_local_execution());
    assert!(decision.override_authorization.authorized);
    assert_eq!(
        decision.override_authorization.authority.as_deref(),
        Some("operator --placement local")
    );
}

/// Batch fanout is the documented one-command path for a wave, so it resolves
/// Lab placement through the same contract as a single cook.
#[test]
fn split_placement_covers_batch_fanout_run_plan_coordinators() {
    let batch = Cli::parse_from([
        "homeboy",
        "--placement",
        "lab",
        "agent-task",
        "fanout",
        "cook-batch",
        "https://github.com/o/r/issues/1",
        "https://github.com/o/r/issues/2",
        "--repo",
        "r",
        "--verify",
        "cargo test",
        "--run-plan",
    ]);

    assert_eq!(
        split_placement_coordinator_label(&batch.command),
        Some("agent-task fanout cook-batch --run-plan")
    );
    assert!(split_placement_lab_runner_unavailable_error(
        &batch.command,
        batch.placement,
        None,
        None,
    )
    .is_some());
}
