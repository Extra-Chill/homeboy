#![cfg(test)]

use super::*;

#[test]
fn detached_planless_handoff_persists_explicit_bench_label_before_handoff() {
    crate::test_support::with_isolated_home(|_| {
        let handoff = materialize_generic_detached_lab_handoff(&[
            "homeboy".to_string(),
            "bench".to_string(),
            "--run-id".to_string(),
            "ssi-fixture-37-20260727-runtime-fixed".to_string(),
        ])
        .expect("persist detached bench handoff");

        assert_eq!(handoff.run_id, "ssi-fixture-37-20260727-runtime-fixed");
        let record = agent_task_lifecycle::status(&handoff.run_id)
            .expect("interrupted caller leaves a discoverable run");
        assert!(!record.state.is_terminal());
        assert_eq!(record.plan_id, handoff.plan.plan_id);
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
        let first = materialize_generic_detached_lab_handoff(&args).expect("first handoff");
        let second = materialize_generic_detached_lab_handoff(&args).expect("replayed handoff");

        assert_eq!(first.run_id, second.run_id);
        assert_eq!(first.plan.plan_id, second.plan.plan_id);
        assert!(agent_task_lifecycle::status(&first.run_id).is_ok());
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

    assert_eq!(route_after_parse(&cli, &normalized, None).unwrap(), None);
}
use clap::Parser;
use homeboy::command_contract::{lab_runner_supports_contract_label, LabCommandPortability};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use tempfile::tempdir;

use super::*;

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

    let routed = inline_test_settings_profiles(&cli, &args).expect("portable settings args");

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

    let error = inline_test_settings_profiles(&cli, &args).expect_err("credential profile refused");

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
            allow_local_fallback: false,
            allow_dirty_lab_workspace: false,
            skip_deps_hydration: false,
            detach_after_handoff: false,
            source_path: None,
            job_overrides: runners::LabJobOverrides::default(),
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
        let record = agent_task_lifecycle::status("cook-preacceptance-order")
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
    });
}

#[test]
fn non_lab_command_continues_local_dispatch() {
    // route_after_parse mutates the process-global LAB_OFFLOAD_METADATA_ENV,
    // so hold the env lock to serialize against tests that assert on it.
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let cli = Cli::parse_from(["homeboy", "status"]);

    let outcome = route_after_parse(&cli, &["homeboy".into(), "status".into()], None).unwrap();

    assert_eq!(outcome, None);
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
        route_after_parse(
            &cli,
            &normalized
                .iter()
                .map(|arg| arg.to_string())
                .collect::<Vec<_>>(),
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

        let outcome = route_after_parse(&cli, &normalized, Some(&output.to_string_lossy()))
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
        let input = homeboy::core::deferred_workload::DeferredWorkloadInput {
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
            test_requirements: homeboy::core::deferred_workload::DeferredWorkloadRequirements {
                required_runtimes: ["homeboy".to_string()].into(),
                required_capabilities: Default::default(),
            },
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
        let deferred = homeboy::core::deferred_workload::defer(input).expect("defer fixture");
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
    let mut record = homeboy::core::deferred_workload::DeferredWorkload {
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
        test_requirements: homeboy::core::deferred_workload::DeferredWorkloadRequirements {
            required_runtimes: ["homeboy".to_string()].into(),
            required_capabilities: Default::default(),
        },
        job_overrides: Default::default(),
        state: homeboy::core::deferred_workload::DeferredWorkloadState::Deferred,
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
        let deferred = homeboy::core::deferred_workload::defer(
            deferred_workload_input(
                &cli,
                &[
                    "homeboy".to_string(),
                    "review".to_string(),
                    "test".to_string(),
                ],
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
            homeboy::core::deferred_workload::records().expect("records")[0].state,
            homeboy::core::deferred_workload::DeferredWorkloadState::Dispatched
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

    let outcome = route_after_parse(&cli, &normalized, None).unwrap();

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

    let outcome = route_after_parse(&cli, &normalized, None)
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

    let err = crate::test_support::with_isolated_home(|_| {
        route_after_parse(&cli, &normalized, None)
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
        route_after_parse(&cli, &normalized, None).expect("managed context"),
        None
    );
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
        route_after_parse(&cli, &normalized, None).expect("runner-resident handoff stays local"),
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
        route_after_parse(&cli, &normalized, None).expect("runner-resident cook stays local"),
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
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "--placement".to_string(),
        "lab".to_string(),
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
        route_after_parse(&cli, &normalized, None)
            .expect("managed promotion must execute on its authorized runner"),
        None
    );
}

#[test]
fn unmanaged_explicit_lab_handoff_keeps_runner_connection_requirements() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::runner::RUNNER_HOSTED_EXEC_ENV, None),
        (homeboy::runner::RUNNER_PLACEMENT_RESOLVED_ENV, None),
        (homeboy::runner::RUNNER_ID_ENV, None),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "disconnected-lab".to_string(),
        "--placement".to_string(),
        "lab".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        r#"{"plan_id":"handoff","tasks":[]}"#.to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let error = crate::test_support::with_isolated_home(|_| {
        route_after_parse(&cli, &normalized, None)
            .expect_err("unmanaged run-plan must still require a Lab runner")
    });

    assert_eq!(error.code.as_str(), "runner.not_found");
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

    let outcome = route_after_parse(&cli, &normalized, None)
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
fn lab_cook_materializes_goal_and_explicit_task_as_one_durable_cell() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path());
        let workspace = workspace.path().display().to_string();
        let cook = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--goal",
            "Preserve one provider cell",
            "--task",
            "Repair the Lab Cook compiler",
            "--cwd",
            &workspace,
            "--to-worktree",
            &workspace,
            "--backend",
            "fixture",
            "--no-finalize",
            "--run-id",
            "cook-lab-goal-task",
        ]);

        let plan = materialize_agent_task_cook_plan(&cook)
            .expect("materialize Lab Cook plan")
            .expect("Cook plan");
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.options.execution_budget.max_provider_executions, 1);
        assert_eq!(plan.metadata["cook_goal"], "Preserve one provider cell");
        assert_eq!(plan.tasks[0].instructions, "Repair the Lab Cook compiler");
        assert_eq!(
            plan.tasks[0].metadata["cook_goal"],
            "Preserve one provider cell"
        );

        agent_task_lifecycle::submit_plan(&plan, Some("cook-lab-goal-task"))
            .expect("persist Cook plan");
        let retry_args = [
            "homeboy",
            "agent-task",
            "retry",
            "cook-lab-goal-task",
            "--run",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let retry = Cli::parse_from(&retry_args);
        let handoff = materialize_agent_task_retry_handoff(&retry, &retry_args)
            .expect("materialize retry handoff")
            .expect("retry handoff");
        // The Homeboy projection is rebuilt when loading durable JSON; verify
        // the provider-cell contract, which is what retry/resume dispatches.
        assert_eq!(handoff.plan.tasks, plan.tasks);
        assert_eq!(handoff.plan.options, plan.options);
        assert_eq!(handoff.plan.metadata, plan.metadata);
    });
}

#[test]
fn lab_run_retry_keeps_a_retryable_cook_failure_attached_to_its_recipe() {
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
        let options = crate::agents::agent_task_service::AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(),
            initial_run_id: run_id.to_string(),
            initial_plan: plan.clone(),
            to_worktree: workspace.path().display().to_string(),
            source_worktree_path: Some(workspace.path().to_path_buf()),
            provider_command: None,
            provider_invocation: None,
            gates: Default::default(),
            max_attempts: 2,
            no_finalize: true,
            base: "main".to_string(),
            task_base_sha: None,
            head: None,
            title: "Lab Cook retry".to_string(),
            commit_message: "Lab Cook retry".to_string(),
            source_refs: Vec::new(),
            protected_branches: Vec::new(),
            ai_tool: "fixture".to_string(),
            ai_model: None,
            ai_used_for: "test".to_string(),
            attempt_dispatcher: None,
            harvest_context:
                homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                    .expect("harvest context"),
        };
        crate::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist Cook recipe");
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("persist Cook attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("bind Cook attempt");
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
        let handoff = materialize_agent_task_retry_handoff(&Cli::parse_from(&args), &args)
            .expect("materialize Cook retry handoff")
            .expect("Cook retry handoff");

        assert!(handoff.run_id.starts_with(&format!("{cook_id}-attempt-2-")));
        let replacement = agent_task_lifecycle::status(&handoff.run_id)
            .expect("Cook-aware Lab retry is persisted");
        assert_eq!(replacement.metadata["cook_id"], cook_id);
        assert_eq!(replacement.metadata["cook_attempt"], 2);
    });
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
        let replacement = agent_task_lifecycle::status(&handoff.run_id).expect("replacement");
        assert_eq!(replacement.metadata["retry_of"], "failed-run");
        assert_eq!(replacement.metadata["retried_from"], "failed-run");
        assert_eq!(replacement.metadata["retry_root"], "failed-run");
        assert_eq!(
            agent_task_lifecycle::status("failed-run")
                .expect("source retry lineage")
                .metadata["retries"],
            serde_json::json!(["failed-run-retry-on-lab"])
        );

        // The Lab executes `run-plan` against the durable replacement. Its
        // re-submission must retain retry lineage so later reconciliation can
        // still resolve the retry reservation through the controller store.
        agent_task_lifecycle::submit_plan(&handoff.plan, Some(&handoff.run_id))
            .expect("resubmit replacement from Lab handoff");
        let resubmitted =
            agent_task_lifecycle::status(&handoff.run_id).expect("resubmitted replacement");
        assert_eq!(resubmitted.metadata["retry_of"], "failed-run");
        assert_eq!(resubmitted.metadata["retried_from"], "failed-run");
        assert_eq!(resubmitted.metadata["retry_root"], "failed-run");
        assert_eq!(
            agent_task_lifecycle::status("failed-run")
                .expect("source retry lineage remains idempotent")
                .metadata["retries"],
            serde_json::json!(["failed-run-retry-on-lab"])
        );

        stage_retry_lab_handoff_before_preacceptance(Some(&handoff), Some("homeboy-lab"))
            .expect("stage replacement handoff before Lab preacceptance");
        let replacement = agent_task_lifecycle::status(&handoff.run_id)
            .expect("staged replacement remains inspectable");
        let staged_handoff = replacement
            .lab_handoff
            .expect("replacement has pending controller handoff");
        assert_eq!(staged_handoff.runner_id, "homeboy-lab");
        let staged_handoff =
            serde_json::to_value(staged_handoff).expect("serialize staged retry handoff");
        assert_eq!(staged_handoff["state"], "pending");
        assert_eq!(staged_handoff["authority"], "controller");

        let error = persist_retry_handoff_preacceptance_failure(
            &handoff,
            Error::internal_unexpected("runner preflight rejected the handoff"),
        );
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("agent-task retry")
                && hint.message.contains(&handoff.run_id)));
        let replacement = agent_task_lifecycle::status(&handoff.run_id).expect("failed retry");
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

    assert!(
        run_split_placement_cook(&cli, &[], None, Some("homeboy-lab"))
            .expect("local placement bypasses Lab cook dispatch")
            .is_none()
    );
}

#[test]
fn cook_to_worktree_provider_workspace_survives_failed_attempt_and_lab_retry() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path());
        let provider_dir = tempfile::tempdir().expect("provider dir");
        let provider_workspace = provider_dir.path().join("worktree");
        let commit = Command::new("git")
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
            .current_dir(workspace.path())
            .output()
            .expect("create fixture commit");
        assert!(commit.status.success(), "{:?}", commit);
        let worktree = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "cook-six-fixture-generic-parity",
                provider_workspace
                    .to_str()
                    .expect("utf8 provider workspace"),
            ])
            .current_dir(workspace.path())
            .output()
            .expect("create linked provider workspace");
        assert!(worktree.status.success(), "{:?}", worktree);
        let provider = provider_dir.path().join("provider");
        let payload = serde_json::json!({
            "worktrees": [{
                "handle": "blocks-engine@cook-six-fixture-generic-parity",
                "path": provider_workspace,
                "branch": "cook-six-fixture-generic-parity",
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
            "blocks-engine",
            "--head",
            "cook-six-fixture-generic-parity",
            "--task-url",
            "https://example.com/tasks/cook-six-fixture-generic-parity",
            "--to-worktree",
            "blocks-engine@cook-six-fixture-generic-parity",
            "--verify",
            "true",
            "--backend",
            "fixture",
            "--prompt",
            "retry this task",
        ]);
        let plan = materialize_agent_task_cook_plan(&cook_cli)
            .expect("materialize cook plan")
            .expect("cook plan");
        let expected_root = provider_workspace.display().to_string();
        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some(expected_root.as_str())
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
        let handoff = materialize_agent_task_retry_handoff(&retry_cli, &retry_args)
            .expect("materialize retry handoff")
            .expect("retry handoff");

        assert_eq!(
            handoff.primary_workspace,
            provider_workspace.canonicalize().expect("root")
        );
        assert_eq!(
            handoff.plan.tasks[0].workspace.root.as_deref(),
            Some(expected_root.as_str())
        );
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
        assert!(agent_task_lifecycle::status("failed-run-retry-1").is_err());
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
        assert!(agent_task_lifecycle::status("failed-run-retry-1").is_err());
    });
}

#[test]
fn agent_task_fanout_dispatch_id_uses_explicit_or_stable_default() {
    let cli = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "fanout",
        "cook-batch",
        "--repo",
        "homeboy",
        "--fanout-id",
        "wave-7167",
        "--verify",
        "cargo test --lib",
        "--run-plan",
        "https://github.com/Extra-Chill/homeboy/issues/7167",
    ]);
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command:
            crate::commands::agent_task::AgentTaskCommand::Fanout(
                crate::commands::agent_task::AgentTaskFanoutArgs {
                    command: crate::commands::agent_task::AgentTaskFanoutCommand::CookBatch(args),
                },
            ),
    }) = cli.command
    else {
        panic!("cook-batch command");
    };

    assert_eq!(agent_task_fanout_cook_batch_dispatch_id(&args), "wave-7167");

    let mut default_args = args;
    default_args.fanout_id = None;
    assert_eq!(
        agent_task_fanout_cook_batch_dispatch_id(&default_args),
        "cook-batch-homeboy-issue-7167-1"
    );
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
    let readiness = runners::LabRunnerReadiness {
        state: runners::LabRunnerReadinessState::Stale,
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
    let readiness = runners::LabRunnerReadiness {
        state: runners::LabRunnerReadinessState::Disconnected,
        selected_runner_id: None,
        available_runner_ids: vec!["homeboy-lab".to_string()],
        reasons: vec!["homeboy-lab is disconnected".to_string()],
        remediation_commands: vec!["homeboy runner connect homeboy-lab".to_string()],
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
        problem.contains("disconnected"),
        "the readiness verdict is the real cause: {problem}"
    );
    let hints = error.details["tried"]
        .as_array()
        .expect("remediation hints")
        .iter()
        .filter_map(|hint| hint.as_str())
        .collect::<Vec<_>>();
    assert!(
        hints
            .iter()
            .any(|hint| hint.contains("homeboy runner connect homeboy-lab")),
        "readiness remediation must be carried through: {hints:?}"
    );
    assert!(
        hints
            .iter()
            .any(|hint| hint.contains("`--placement lab` is the supported spelling")),
        "remediation must confirm the documented spelling: {hints:?}"
    );
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
