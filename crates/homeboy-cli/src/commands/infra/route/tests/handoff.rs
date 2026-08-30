#![cfg(test)]

use super::*;
use clap::Parser;
use std::fs;
use tempfile::tempdir;

fn capture_fixture_preflight(cli: &Cli, normalized: &[String]) {
    homeboy::core::parsed_command_preflight::reset_captured_result_for_test();
    homeboy::core::parsed_command_preflight::capture_result(
        homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult::new(
            normalized.to_vec(),
            resource_policy::parsed_command_preflight_input(cli, normalized),
            None,
            None,
            homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::NotApplicable,
            homeboy::core::parsed_command_preflight::FallbackDirective::None,
            crate::cli_runtime::placement_directive(cli, cli.runner.as_deref(), false),
            cli.runner.clone(),
        ),
    );
}

#[test]
fn rig_install_offload_translates_source_path_instead_of_forwarding_it() {
    let source_dir = tempdir().expect("source dir");
    let source_path = source_dir
        .path()
        .canonicalize()
        .expect("canonical temp dir")
        .join("static-site-importer");
    fs::create_dir_all(&source_path).expect("create source package");
    let local_source = source_path.to_string_lossy().to_string();
    let command = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        local_source.clone(),
        "--reinstall".to_string(),
    ];

    let sync_root = rig_install_source_sync_root(&command).expect("sync root");
    let remote_root = "/home/runner/Developer/_lab_workspaces/static-site-importer-abc";
    let translated = translate_command_path_prefix(&command, &sync_root, remote_root);

    // The forwarded source must be the runner-side path, never the
    // controller-local path that broke `rig install --runner` (#6964).
    assert_eq!(translated[3], remote_root);
    assert!(
        !translated.iter().any(|arg| arg.contains(&local_source)),
        "controller-local source path must not be forwarded: {translated:?}"
    );
}

#[test]
fn linked_local_rig_check_stays_local_without_runner() {
    // Scope the offload-metadata env var so a parallel test that sets it
    // (process-global) cannot leak into this local/no-runner assertion.
    let temp_home = tempdir().expect("temp home");
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        ("HOME", Some(temp_home.path().to_str().expect("home path"))),
    ]);
    write_rig_source_metadata(temp_home.path(), "linked-local", true);
    let normalized = vec![
        "homeboy".to_string(),
        "rig".to_string(),
        "check".to_string(),
        "linked-local".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);
    capture_fixture_preflight(&cli, &normalized);

    let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None)
        .expect("linked local rig check should skip automatic Lab offload");

    assert_eq!(outcome, None);
    assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
}

#[test]
fn installed_git_rig_check_keeps_default_lab_offload() {
    let temp_home = tempdir().expect("temp home");
    let _home = EnvGuard::set("HOME", temp_home.path().to_str().expect("home path"));
    write_rig_source_metadata(temp_home.path(), "installed-git", false);
    let cli = Cli::parse_from(["homeboy", "rig", "check", "installed-git"]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "rig check");
    assert!(command.is_portable());
    assert!(command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.infer_source_path_tools);
}

#[test]
fn lab_command_preserves_portable_contract_shape() {
    let cli = Cli::parse_from(["homeboy", "review", "lint"]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "review lint");
    assert!(command.is_portable());
    assert!(command.routing_policy.requires_extension_parity);
}

#[test]
fn extension_update_routes_locally_without_explicit_lab_runner() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let normalized = vec![
        "homeboy".to_string(),
        "extension".to_string(),
        "update".to_string(),
        "wordpress".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);
    capture_fixture_preflight(&cli, &normalized);

    let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None)
        .expect("extension update without --runner should not offload");

    assert_eq!(outcome, None);
    assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
}

#[test]
fn extension_dev_run_keeps_its_runner_workflow_on_the_controller() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let normalized = vec![
        "homeboy".to_string(),
        "extension".to_string(),
        "dev-run".to_string(),
        "wordpress".to_string(),
        "--source".to_string(),
        "/tmp/wordpress".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "homeboy".to_string(),
        "extension".to_string(),
        "show".to_string(),
        "wordpress".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);
    capture_fixture_preflight(&cli, &normalized);

    let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None)
        .expect("dev-run should execute its own runner lifecycle");

    assert_eq!(outcome, None);
    assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
}

#[test]
fn fuzz_doctor_routes_locally_without_explicit_lab_runner() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let normalized = vec![
        "homeboy".to_string(),
        "fuzz".to_string(),
        "doctor".to_string(),
        "--extension".to_string(),
        "nodejs".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);
    capture_fixture_preflight(&cli, &normalized);

    let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None)
        .expect("fuzz doctor without --runner should remain a local diagnostic");

    assert_eq!(outcome, None);
    assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
}

#[test]
fn global_runner_for_runs_show_has_local_mirror_guidance() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "runs",
        "show",
        "run-123",
    ]);

    let err = route_after_parse_with_provenance(
        &cli,
        &[
            "homeboy".into(),
            "--runner".into(),
            "homeboy-lab".into(),
            "runs".into(),
            "show".into(),
            "run-123".into(),
        ],
        None,
        None,
    )
    .expect_err("runs show rejects global runner with guidance");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("homeboy runs show run-123"));
    assert!(err.message.contains("without --runner"));
}

#[test]
fn runs_list_runner_option_after_subcommand_routes_locally() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);

    for normalized in [
        vec![
            "homeboy".to_string(),
            "runs".to_string(),
            "list".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--status".to_string(),
            "running".to_string(),
            "--limit".to_string(),
            "20".to_string(),
        ],
        vec![
            "homeboy".to_string(),
            "runs".to_string(),
            "list".to_string(),
            "--runner=homeboy-lab".to_string(),
            "--status".to_string(),
            "running".to_string(),
            "--limit".to_string(),
            "20".to_string(),
        ],
    ] {
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None)
            .expect("runs list subcommand runner option should not be rejected");

        assert_eq!(outcome, None);
    }
}

#[test]
fn global_runner_for_runs_list_keeps_placement_guidance() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let normalized = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "runs".to_string(),
        "list".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let err = route_after_parse_with_provenance(&cli, &normalized, None, None)
        .expect_err("top-level runner on runs list should keep placement guidance");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err
        .message
        .contains("homeboy runs list --runner homeboy-lab"));
}

#[test]
fn runs_artifact_attach_runner_option_routes_locally() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);

    for normalized in [
        vec![
            "homeboy".to_string(),
            "runs".to_string(),
            "artifact".to_string(),
            "attach".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--path".to_string(),
            "/tmp/matrix-summary.json".to_string(),
            "--name".to_string(),
            "matrix-summary".to_string(),
            "run-123".to_string(),
        ],
        vec![
            "homeboy".to_string(),
            "runs".to_string(),
            "artifact".to_string(),
            "attach".to_string(),
            "--runner=homeboy-lab".to_string(),
            "--path=/tmp/matrix-summary.json".to_string(),
            "--name=matrix-summary".to_string(),
            "run-123".to_string(),
        ],
    ] {
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None)
            .expect("runs artifact attach command-local runner option should not be rejected");

        assert_eq!(outcome, None);
    }
}

#[test]
fn agent_task_cook_keeps_its_coordinator_local_for_all_placements() {
    let automatic = Cli::parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "homeboy@cook-routing",
        "--verify",
        "cargo test --locked",
    ]);
    let automatic_command = lab_offload_command(&automatic.command).unwrap().unwrap();
    assert!(!automatic_command.is_portable());
    assert!(!automatic_command.routing_policy.default_lab_offload);

    let explicit = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "homeboy@cook-routing",
        "--verify",
        "cargo test --locked",
    ]);
    let explicit_command = lab_offload_command(&explicit.command).unwrap().unwrap();
    assert_eq!(explicit.runner.as_deref(), Some("homeboy-lab"));
    assert!(!explicit_command.is_portable());

    let lab = Cli::parse_from([
        "homeboy",
        "--placement",
        "lab",
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "homeboy@cook-routing",
        "--verify",
        "cargo test --locked",
    ]);
    assert!(!lab_offload_command(&lab.command)
        .unwrap()
        .unwrap()
        .is_portable());
    assert_eq!(lab.placement, crate::cli_surface::Placement::Lab);
}

#[test]
fn unscoped_provider_discovery_does_not_offload() {
    // Controller and runner have different extensions, runtime defaults,
    // secrets, and provider readiness, so relocating an unscoped read changes
    // the meaning of its answer. Offload stays opt-in (#9763).
    let cli = Cli::parse_from(["homeboy", "agent-task", "providers"]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(cli.runner, None);
    assert_eq!(cli.placement, crate::cli_surface::Placement::Auto);
    assert!(!command.routing_policy.default_lab_offload);
    assert_eq!(
        command.source_path_mode,
        runners::LabOffloadSourcePathMode::RunnerResident,
        "runner-resident source mode is what exempts this read from warm-controller offload promotion"
    );
}

#[test]
fn default_placement_provider_discovery_stays_local_despite_connected_default_runner() {
    // Exercise the same compiled default-placement provenance as the CLI
    // runtime. A captured hot decision with a connected default runner is the
    // generic route state that previously bypassed the provider contract.
    crate::test_support::with_isolated_home(|_| {
        homeboy::core::resource_policy_context::reset_captured_context_for_test();
        homeboy::core::resource_policy_context::capture_context(
            homeboy::core::resource_policy_context::ResourcePolicyContext {
                command: "agent-task providers".to_string(),
                severity: "hot".to_string(),
                local_override: false,
                warned: true,
                message: None,
                runner_selection:
                    homeboy::core::resource_policy_context::ResourcePolicyRunnerSelection {
                        runner_id: Some("homeboy-lab".to_string()),
                        available_runner_ids: vec!["homeboy-lab".to_string()],
                        readiness_state: "connected_ready".to_string(),
                        readiness_reasons: Vec::new(),
                        remediation_commands: Vec::new(),
                        reason: "default_lab_runner".to_string(),
                    },
                host: homeboy::core::resource_policy_context::ResourcePolicyHostSnapshot {
                    load_severity: "hot".to_string(),
                    load_one: None,
                    load_five: None,
                    load_fifteen: None,
                    cpu_count: 1,
                    memory_severity: None,
                    memory_used_percent: None,
                    memory_available_mb: None,
                    memory_total_mb: None,
                    relevant_process_count: 0,
                    process_severity: "ok".to_string(),
                    active_rig_lease_count: 0,
                    rig_lease_severity: "ok".to_string(),
                    rig_lease_concurrency_limit: None,
                },
            },
        );
        let matches = Cli::command_with_scoped_lab_args()
            .try_get_matches_from(["homeboy", "agent-task", "providers"])
            .expect("bare provider discovery parses");
        let (compiled, _) = Cli::compile_registered_arg_matches(&matches)
            .expect("provider discovery compiles with provenance");

        assert_eq!(
            compiled.provenance.source("placement"),
            Some(crate::cli_surface::ArgumentSource::Default)
        );
        assert_eq!(compiled.value.runner, None);
        assert_eq!(
            compiled.value.placement,
            crate::cli_surface::Placement::Auto
        );
        assert_eq!(
            route_after_parse_with_provenance(
                &compiled.value,
                &[
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "providers".to_string()
                ],
                None,
                Some(&compiled.provenance),
            )
            .expect("unscoped discovery must not select the default runner"),
            None
        );
        homeboy::core::resource_policy_context::reset_captured_context_for_test();
    });
}

#[test]
fn hot_cook_with_explicit_lab_placement_uses_the_admitted_ready_runner() {
    homeboy::core::resource_policy_context::reset_captured_context_for_test();
    homeboy::core::parsed_command_preflight::reset_captured_result_for_test();
    let context = homeboy::core::resource_policy_context::ResourcePolicyContext {
        command: "agent-task cook".to_string(),
        severity: "hot".to_string(),
        local_override: false,
        warned: true,
        message: None,
        runner_selection: homeboy::core::resource_policy_context::ResourcePolicyRunnerSelection {
            runner_id: Some("admitted-lab".to_string()),
            available_runner_ids: vec!["admitted-lab".to_string()],
            readiness_state: "connected_ready".to_string(),
            readiness_reasons: Vec::new(),
            remediation_commands: Vec::new(),
            reason: "default_lab_runner".to_string(),
        },
        host: homeboy::core::resource_policy_context::ResourcePolicyHostSnapshot {
            load_severity: "hot".to_string(),
            load_one: None,
            load_five: None,
            load_fifteen: None,
            cpu_count: 1,
            memory_severity: None,
            memory_used_percent: None,
            memory_available_mb: None,
            memory_total_mb: None,
            relevant_process_count: 0,
            process_severity: "ok".to_string(),
            active_rig_lease_count: 0,
            rig_lease_severity: "ok".to_string(),
            rig_lease_concurrency_limit: None,
        },
    };
    let cli = Cli::parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--placement",
        "lab",
        "--to-worktree",
        "repo@hot-cook",
    ]);
    homeboy::core::parsed_command_preflight::capture_result(
        homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult::new(
            vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ],
            resource_policy::parsed_command_preflight_input(
                &cli,
                &[
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                ],
            ),
            Some(context),
            Some(
                homeboy::core::parsed_command_preflight::LabReadinessSnapshot {
                    state: "connected_ready".to_string(),
                    selected_runner_id: Some("admitted-lab".to_string()),
                    available_runner_ids: vec!["admitted-lab".to_string()],
                    reasons: Vec::new(),
                    remediation_commands: Vec::new(),
                    repair_admitted_runner_ids: Vec::new(),
                },
            ),
            homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::NotApplicable,
            homeboy::core::parsed_command_preflight::FallbackDirective::None,
            crate::cli_runtime::placement_directive(&cli, Some("admitted-lab"), false),
            Some("admitted-lab".to_string()),
        ),
    );
    let preflight =
        homeboy::core::parsed_command_preflight::captured_result().expect("fixture preflight");
    assert_eq!(
        preflight
            .placement
            .runner
            .as_ref()
            .expect("hot controller admission retains the ready runner")
            .runner_id,
        "admitted-lab"
    );
    let decision = finalize_placement(&preflight.placement, "cook", None);
    assert_eq!(decision.requested, crate::cli_surface::Placement::Lab);
    assert_eq!(
        decision.selected,
        homeboy_lab_runner_contract::EffectiveExecutionPlacement::Lab
    );
    assert_eq!(
        decision.runner.expect("selected runner").runner_id,
        "admitted-lab"
    );
    homeboy::core::resource_policy_context::reset_captured_context_for_test();
    homeboy::core::parsed_command_preflight::reset_captured_result_for_test();
}

#[test]
fn agent_task_controller_run_from_spec_supports_lab_placement_runner_routing() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "agent-task",
        "controller",
        "run-from-spec",
        "loop.json",
        "--max-actions",
        "1",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();
    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert_eq!(cli.placement, crate::cli_surface::Placement::Auto);
    assert_eq!(
        command.hot_label,
        "agent-task controller from-spec --resume/run-from-spec/materialize"
    );
    assert!(command.is_portable());
    assert!(command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.requires_extension_parity);
    assert_eq!(
        command.workspace_mode_policy,
        runners::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
    );
}

#[test]
fn agent_task_controller_materialization_family_auto_selects_default_lab_runner() {
    for args in [
        [
            "homeboy",
            "agent-task",
            "controller",
            "from-spec",
            "loop.json",
            "--resume",
            "--max-actions",
            "1",
        ]
        .as_slice(),
        [
            "homeboy",
            "agent-task",
            "controller",
            "run-from-spec",
            "loop.json",
            "--max-actions",
            "1",
        ]
        .as_slice(),
        [
            "homeboy",
            "agent-task",
            "controller",
            "materialize",
            "loop.json",
        ]
        .as_slice(),
    ] {
        let cli = Cli::parse_from(args);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(
            command.hot_label,
            "agent-task controller from-spec --resume/run-from-spec/materialize"
        );
        assert!(command.is_portable());
        assert!(command.routing_policy.default_lab_offload);
        assert!(command.routing_policy.infer_source_path_tools);
        assert!(!command.routing_policy.requires_extension_parity);
        assert_eq!(
            command.workspace_mode_policy,
            runners::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
        );
    }
}

#[test]
fn agent_task_fanout_submit_batch_requires_explicit_runner_under_lab_placement() {
    // Isolate from a parallel test leaking the offload-metadata env var,
    // which would otherwise short-circuit route_after_parse as a Lab
    // offload subprocess and return Ok(None) instead of the deny error.
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let normalized = vec![
        "homeboy".to_string(),
        "--placement".to_string(),
        "lab".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "submit-batch".to_string(),
        "--input".to_string(),
        "fanout.json".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);
    capture_fixture_preflight(&cli, &normalized);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();
    assert_eq!(command.hot_label, "agent-task fanout submit-batch");
    assert!(!command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.infer_source_path_tools);

    let err = route_after_parse_with_provenance(&cli, &normalized, None, None)
        .expect_err("fanout submit-batch must not run locally under Lab placement");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    // submit-batch needs an explicit runner under Lab placement: it does
    // not auto-offload, so `--placement lab` without an eligible runner is
    // refused rather than silently running locally.
    assert!(err.message.contains("--placement lab"));
    assert!(err.message.contains("requires an eligible Lab runner"));
    assert!(err.message.contains("agent-task fanout submit-batch"));
}

#[test]
fn agent_task_fanout_cook_batch_run_plan_keeps_cook_coordinators_local() {
    // `--runner` implies Lab placement and conflicts with an explicit
    // `--placement` at parse time (#9002); the runner-pinned coordinator uses
    // `--runner` alone.
    let normalized = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "cook-batch".to_string(),
        "--repo".to_string(),
        "homeboy".to_string(),
        "--verify".to_string(),
        "cargo test --locked agent_task".to_string(),
        "--run-plan".to_string(),
        "https://github.com/Extra-Chill/homeboy/issues/7011".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();
    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert_eq!(command.hot_label, "agent-task fanout cook-batch");
    assert!(!command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
    // Controller coordination stays local while typed child attempts retain
    // the selected Lab runner and its normal placement enforcement (#8519).
    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
}

#[test]
fn agent_task_fanout_state_reads_are_runner_resident() {
    for args in [
        [
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "fanout",
            "status",
            "fanout-batch-123",
        ],
        [
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "fanout",
            "artifacts",
            "fanout-batch-123",
        ],
    ] {
        let cli = Cli::parse_from(args);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert_eq!(command.hot_label, "agent-task fanout status/artifacts");
        assert!(command.is_portable());
        assert!(!command.routing_policy.default_lab_offload);
        assert_eq!(
            command.source_path_mode,
            runners::LabOffloadSourcePathMode::RunnerResident
        );
        assert_eq!(
            command.workspace_mode_policy,
            runners::LabOffloadWorkspaceModePolicy::RunnerResident
        );
        assert!(command.required_extensions.is_empty());
        assert!(!command.routing_policy.requires_extension_parity);
        assert!(!command.routing_policy.infer_source_path_tools);
    }
}

#[test]
fn tunnel_service_start_supports_explicit_runner_discovery() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "tunnel",
        "service",
        "start",
        "preview",
        "--cwd",
        "/home/user/Developer/_lab_workspaces/site",
        "--command",
        "npm run dev",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert_eq!(command.hot_label, "tunnel service start");
    assert!(command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
    assert_eq!(
        command.source_path_mode,
        runners::LabOffloadSourcePathMode::RunnerResident
    );
    assert_eq!(
        command.workspace_mode_policy,
        runners::LabOffloadWorkspaceModePolicy::RunnerResident
    );
    assert!(!command.routing_policy.requires_extension_parity);
    assert!(command.required_extensions.is_empty());
    assert!(!command.routing_policy.infer_source_path_tools);
}

#[test]
fn tunnel_preview_consumer_run_keeps_explicit_runner_contract() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "tunnel",
        "preview-consumer",
        "run",
        "--config",
        "consumer.json",
        "--preview-public-url",
        "https://preview.example.test/run",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "tunnel preview-consumer run");
    assert!(command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
}

#[test]
fn lab_command_with_mutation_flag_stays_portable_for_patch_capture() {
    let cli = Cli::parse_from(["homeboy", "review", "audit", "--baseline"]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "review audit");
    assert!(command.is_portable());
    assert!(command.routing_policy.requires_extension_parity);
}

#[test]
fn lab_command_with_ratchet_stays_portable_for_patch_capture() {
    let cli = Cli::parse_from(["homeboy", "review", "audit", "--ratchet"]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "review audit");
    assert!(command.is_portable());
    assert!(command.routing_policy.requires_extension_parity);
}

#[test]
fn strip_component_target_replaces_positional_with_path() {
    let args = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
        "--fix".to_string(),
        "sample-component".to_string(),
    ];

    let rewritten = strip_component_target_args(&args, "sample-component", "/src/sample");

    assert_eq!(
        rewritten,
        vec![
            "homeboy".to_string(),
            "review".to_string(),
            "lint".to_string(),
            "--fix".to_string(),
            "--path".to_string(),
            "/src/sample".to_string(),
        ]
    );
}

#[test]
fn strip_component_target_replaces_component_flag_with_path() {
    let args = vec![
        "homeboy".to_string(),
        "refactor".to_string(),
        "--from".to_string(),
        "lint".to_string(),
        "--write".to_string(),
        "--component".to_string(),
        "sample-component".to_string(),
    ];

    let rewritten = strip_component_target_args(&args, "sample-component", "/src/sample");

    assert_eq!(
        rewritten,
        vec![
            "homeboy".to_string(),
            "refactor".to_string(),
            "--from".to_string(),
            "lint".to_string(),
            "--write".to_string(),
            "--path".to_string(),
            "/src/sample".to_string(),
        ]
    );
}

#[test]
fn strip_component_target_only_strips_first_positional_match() {
    // A `--from` value equal to the component id must survive; only the bare
    // positional component token is dropped.
    let args = vec![
        "homeboy".to_string(),
        "refactor".to_string(),
        "--from".to_string(),
        "lint".to_string(),
        "--write".to_string(),
        "dmc".to_string(),
    ];

    let rewritten = strip_component_target_args(&args, "dmc", "/src/dmc");

    assert_eq!(
        rewritten,
        vec![
            "homeboy".to_string(),
            "refactor".to_string(),
            "--from".to_string(),
            "lint".to_string(),
            "--write".to_string(),
            "--path".to_string(),
            "/src/dmc".to_string(),
        ]
    );
}

#[test]
fn strip_component_target_preserves_passthrough_args() {
    let args = vec![
        "homeboy".to_string(),
        "lint".to_string(),
        "--fix".to_string(),
        "dmc".to_string(),
        "--".to_string(),
        "dmc".to_string(),
    ];

    let rewritten = strip_component_target_args(&args, "dmc", "/src/dmc");

    assert_eq!(
        rewritten,
        vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--fix".to_string(),
            "--".to_string(),
            "dmc".to_string(),
            "--path".to_string(),
            "/src/dmc".to_string(),
        ]
    );
}

#[test]
fn rewrite_component_target_skips_when_path_override_present() {
    let cli = Cli::parse_from([
        "homeboy",
        "review",
        "lint",
        "--fix",
        "sample-component",
        "--path",
        "/explicit/path",
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
        "--fix".to_string(),
        "sample-component".to_string(),
        "--path".to_string(),
        "/explicit/path".to_string(),
    ];

    assert!(rewrite_component_target_to_path(&cli.command, &normalized).is_none());
}

#[test]
fn rewrite_component_target_skips_without_component() {
    // No positional component and no --path: source resolves from CWD, so
    // there is nothing to rewrite.
    let cli = Cli::parse_from(["homeboy", "review", "lint", "--fix"]);
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
        "--fix".to_string(),
    ];

    assert!(rewrite_component_target_to_path(&cli.command, &normalized).is_none());
}

#[test]
fn lab_route_source_path_args_rewrites_review_lint_component_without_patch_capture() {
    let cli = Cli::parse_from(["homeboy", "review", "lint", "homeboy"]);
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
        "homeboy".to_string(),
    ];

    let rewritten = lab_route_source_path_args(&cli.command, &normalized, false)
        .expect("review lint component id should become a source path");

    assert_eq!(rewritten[0..3], normalized[0..3]);
    assert_eq!(
        rewritten
            .iter()
            .filter(|arg| arg.as_str() == "homeboy")
            .count(),
        1
    );
    assert!(rewritten.contains(&"--path".to_string()));
}

#[test]
fn rewrite_ad_hoc_lab_workspace_adds_path_for_pathless_lint() {
    let dir = tempdir().unwrap();
    let _cwd = CwdGuard::set(dir.path());
    let cwd = std::env::current_dir().expect("current dir");
    let cli = Cli::parse_from(["homeboy", "review", "lint"]);
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
    ];

    let rewritten = rewrite_ad_hoc_lab_workspace_to_path(&cli.command, &normalized)
        .expect("pathless lint should become explicit path");

    assert_eq!(
        rewritten,
        vec![
            "homeboy".to_string(),
            "review".to_string(),
            "lint".to_string(),
            "--path".to_string(),
            cwd.to_string_lossy().to_string(),
        ]
    );
}

#[test]
fn rewrite_ad_hoc_lab_workspace_inserts_path_before_passthrough() {
    let dir = tempdir().unwrap();
    let _cwd = CwdGuard::set(dir.path());
    let cwd = std::env::current_dir().expect("current dir");
    let cli = Cli::parse_from(["homeboy", "review", "test", "--", "--filter", "ExampleTest"]);
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "test".to_string(),
        "--".to_string(),
        "--filter".to_string(),
        "ExampleTest".to_string(),
    ];

    let rewritten = rewrite_ad_hoc_lab_workspace_to_path(&cli.command, &normalized)
        .expect("pathless test should become explicit path");

    assert_eq!(
        rewritten,
        vec![
            "homeboy".to_string(),
            "review".to_string(),
            "test".to_string(),
            "--path".to_string(),
            cwd.to_string_lossy().to_string(),
            "--".to_string(),
            "--filter".to_string(),
            "ExampleTest".to_string(),
        ]
    );
}

#[test]
fn rewrite_ad_hoc_lab_workspace_skips_registered_component_or_path() {
    let component_cli = Cli::parse_from(["homeboy", "review", "lint", "homeboy"]);
    let path_cli = Cli::parse_from(["homeboy", "review", "audit", "--path", "/tmp/homeboy"]);

    assert!(rewrite_ad_hoc_lab_workspace_to_path(
        &component_cli.command,
        &[
            "homeboy".to_string(),
            "review".to_string(),
            "lint".to_string(),
            "homeboy".to_string(),
        ],
    )
    .is_none());
    assert!(rewrite_ad_hoc_lab_workspace_to_path(
        &path_cli.command,
        &[
            "homeboy".to_string(),
            "review".to_string(),
            "audit".to_string(),
            "--path".to_string(),
            "/tmp/homeboy".to_string(),
        ],
    )
    .is_none());
}

/// The run id from #11599's reproduction. A controller-local Cook whose reads
/// all failed with `agent-task run record not found` because they were answered
/// by a runner that never owned the record.
const OWNER_LOCAL_RUN_ID: &str = "agent-task-a7fdaeff-8fb8-4ab2-b7da-d985f4228dcd";

/// The condition under which the regression fired: a connected default Lab
/// runner that readiness happily selects for anything holding a Lab contract.
fn connected_default_lab_runner() -> Option<String> {
    Some("homeboy-lab".to_string())
}

fn route_runner_for(args: &[&str], inferred: &Option<String>) -> Option<String> {
    let cli = Cli::try_parse_from(args).expect("generated command parses");
    let lab_command = lab_offload_command(&cli.command)
        .expect("lab route contract resolves")
        .expect("agent-task commands carry a Lab contract");
    inferred.as_deref().and_then(|runner| {
        (cli.runner.is_some()
            || lab_routing::authorizes_policy_lab_runner(
                &lab_command.command,
                cli.placement,
                lab_routing::captured_pressure_severity().as_deref(),
            ))
        .then(|| runner.to_string())
    })
}

#[test]
fn controller_local_lifecycle_reads_never_acquire_a_default_lab_runner() {
    // These are read-only introspection commands over a durable record this
    // controller owns. Relocating them does not just cost a round trip — it
    // asks a machine that has never seen the record to answer for it, which is
    // exactly the `agent-task run record not found` in #11599.
    let inferred = connected_default_lab_runner();
    for args in [
        ["homeboy", "agent-task", "providers"].as_slice(),
        ["homeboy", "agent-task", "status", OWNER_LOCAL_RUN_ID].as_slice(),
        [
            "homeboy",
            "agent-task",
            "status",
            OWNER_LOCAL_RUN_ID,
            "--full",
        ]
        .as_slice(),
        ["homeboy", "agent-task", "logs", OWNER_LOCAL_RUN_ID].as_slice(),
        ["homeboy", "agent-task", "diagnose", OWNER_LOCAL_RUN_ID].as_slice(),
        ["homeboy", "agent-task", "evidence", OWNER_LOCAL_RUN_ID].as_slice(),
    ] {
        assert_eq!(
            route_runner_for(args, &inferred),
            None,
            "{args:?} must resolve against the owner of its durable record",
        );
    }
}

#[test]
fn controller_local_record_owns_lifecycle_reads_before_default_lab_selection() {
    crate::test_support::with_isolated_home(|_| {
        let inferred = connected_default_lab_runner();
        let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
            "local-owner-routing",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "local-owner-task",
                "executor": { "backend": "fixture" },
                "instructions": "exercise local lifecycle ownership"
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&plan, Some(OWNER_LOCAL_RUN_ID))
            .expect("controller-local record");

        for args in [
            ["homeboy", "agent-task", "status", OWNER_LOCAL_RUN_ID].as_slice(),
            ["homeboy", "agent-task", "logs", OWNER_LOCAL_RUN_ID].as_slice(),
            ["homeboy", "agent-task", "evidence", OWNER_LOCAL_RUN_ID].as_slice(),
            ["homeboy", "agent-task", "diagnose", OWNER_LOCAL_RUN_ID].as_slice(),
            ["homeboy", "agent-task", "review", OWNER_LOCAL_RUN_ID].as_slice(),
            ["homeboy", "agent-task", "reconcile", OWNER_LOCAL_RUN_ID].as_slice(),
        ] {
            let cli = Cli::try_parse_from(args).expect("lifecycle command parses");
            assert!(
                controller_owns_agent_task_lifecycle_command(&cli).expect("owner resolves"),
                "{args:?} must remain controller-local before the connected default Lab is selected"
            );
            let route_runner =
                if controller_owns_agent_task_lifecycle_command(&cli).expect("owner resolves") {
                    None
                } else {
                    route_runner_for(args, &inferred)
                };
            assert_eq!(route_runner, None, "{args:?} must not use homeboy-lab");
        }

        let plan_only_retry =
            Cli::try_parse_from(["homeboy", "agent-task", "retry", OWNER_LOCAL_RUN_ID])
                .expect("plan-only retry parses");
        assert!(
            controller_owns_agent_task_lifecycle_command(&plan_only_retry)
                .expect("plan-only retry owner resolves")
        );

        let executable_retry = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "retry",
            OWNER_LOCAL_RUN_ID,
            "--run",
            "--runner",
            "homeboy-lab",
        ])
        .expect("executable retry parses");
        assert!(
            !controller_owns_agent_task_lifecycle_command(&executable_retry)
                .expect("executable retry owner resolves"),
            "retry --run must reach controller preflight and Lab handoff materialization"
        );
    });
}

#[test]
fn every_generated_next_action_command_round_trips_to_the_record_owner() {
    // A generated next action that walks the operator back into the failure it
    // was printed to resolve is worse than no guidance at all (#11599). Each of
    // these strings is emitted verbatim by a diagnosis/status projection.
    let inferred = connected_default_lab_runner();
    for generated in [
        format!("homeboy agent-task status {OWNER_LOCAL_RUN_ID} --full"),
        format!("homeboy agent-task logs {OWNER_LOCAL_RUN_ID}"),
        format!("homeboy agent-task diagnose {OWNER_LOCAL_RUN_ID}"),
        format!("homeboy agent-task diagnose {OWNER_LOCAL_RUN_ID} --full"),
        format!("homeboy agent-task evidence {OWNER_LOCAL_RUN_ID}"),
    ] {
        let args: Vec<&str> = generated.split_whitespace().collect();
        assert_eq!(
            route_runner_for(&args, &inferred),
            None,
            "`{generated}` must resolve against the controller that owns the record",
        );
    }
}

#[test]
fn explicit_local_placement_keeps_a_retry_controller_owned() {
    // `--placement local` at creation is an ownership statement for the whole
    // lifecycle. Acquiring a Lab runner here is what staged a retry handoff for
    // a controller-local Cook and left its successor unable to decode a
    // canonical placement decision (#11600).
    assert_eq!(
        route_runner_for(
            &[
                "homeboy",
                "--placement",
                "local",
                "agent-task",
                "retry",
                OWNER_LOCAL_RUN_ID,
                "--run",
            ],
            &connected_default_lab_runner(),
        ),
        None,
    );
}

#[test]
fn an_explicit_runner_still_reaches_the_runner_for_provider_discovery() {
    // Pinning is its own authority: a runner catalog read is exactly what
    // `--runner` is for, and #9651/#9763 kept that probe available.
    assert_eq!(
        route_runner_for(
            &[
                "homeboy",
                "--runner",
                "homeboy-lab",
                "agent-task",
                "providers",
            ],
            &connected_default_lab_runner(),
        ),
        Some("homeboy-lab".to_string()),
    );
}

#[test]
fn explicit_lab_placement_still_reaches_the_default_runner() {
    assert_eq!(
        route_runner_for(
            &["homeboy", "--placement", "lab", "agent-task", "providers",],
            &connected_default_lab_runner(),
        ),
        Some("homeboy-lab".to_string()),
    );
}

#[test]
fn genuine_lab_workloads_still_offload_automatically() {
    // The fix must not turn off automatic offload for the commands whose
    // contracts actually declare it.
    let inferred = connected_default_lab_runner();
    for args in [
        ["homeboy", "agent-task", "run-plan", "--plan", "-"].as_slice(),
        [
            "homeboy",
            "agent-task",
            "retry",
            OWNER_LOCAL_RUN_ID,
            "--run",
        ]
        .as_slice(),
    ] {
        assert_eq!(
            route_runner_for(args, &inferred),
            Some("homeboy-lab".to_string()),
            "{args:?} declares automatic Lab offload and must keep it",
        );
    }
}

#[test]
fn no_connected_runner_yields_no_route_runner_regardless_of_contract() {
    assert_eq!(
        route_runner_for(&["homeboy", "agent-task", "run-plan", "--plan", "-"], &None),
        None,
    );
}
