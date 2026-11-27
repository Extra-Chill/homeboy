#![cfg(test)]

use super::*;
use clap::Parser;
use std::fs;
use tempfile::tempdir;

#[must_use = "the captured preflight is scoped to the returned guard"]
fn capture_fixture_preflight(
    cli: &Cli,
    normalized: &[String],
) -> homeboy::core::parsed_command_preflight::CapturedResultTestScope {
    // Scope the capture to this test thread so concurrently running route
    // tests cannot observe (or overwrite) each other's controller decision.
    let scope = homeboy::core::parsed_command_preflight::isolate_captured_result_for_test();
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
    scope
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
    let _preflight = capture_fixture_preflight(&cli, &normalized);

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
fn commands_owning_their_runner_or_local_diagnostics_route_locally() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    for argv in [
        vec!["homeboy", "extension", "update", "wordpress"],
        // dev-run executes its own runner lifecycle.
        vec![
            "homeboy",
            "extension",
            "dev-run",
            "wordpress",
            "--source",
            "/tmp/wordpress",
            "--runner",
            "homeboy-lab",
            "homeboy",
            "extension",
            "show",
            "wordpress",
        ],
        vec!["homeboy", "fuzz", "doctor", "--extension", "nodejs"],
        // Command-local `--runner` options are not global placement.
        vec![
            "homeboy",
            "runs",
            "list",
            "--runner",
            "homeboy-lab",
            "--status",
            "running",
        ],
        vec!["homeboy", "runs", "list", "--runner=homeboy-lab"],
        vec![
            "homeboy",
            "runs",
            "artifact",
            "attach",
            "--runner",
            "homeboy-lab",
            "--path",
            "/tmp/matrix-summary.json",
            "--name",
            "matrix-summary",
            "run-123",
        ],
        vec![
            "homeboy",
            "runs",
            "artifact",
            "attach",
            "--runner=homeboy-lab",
            "--path=/tmp/matrix-summary.json",
            "--name=matrix-summary",
            "run-123",
        ],
    ] {
        let normalized = argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let cli = Cli::parse_from(&normalized);
        let _preflight = capture_fixture_preflight(&cli, &normalized);

        let outcome = route_after_parse_with_provenance(&cli, &normalized, None, None)
            .unwrap_or_else(|error| panic!("{argv:?} must route locally: {error}"));

        assert_eq!(outcome, None, "{argv:?}");
        assert!(
            std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err(),
            "{argv:?}"
        );
    }
}

#[test]
fn global_runner_on_runs_reads_is_rejected_with_local_guidance() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    for (argv, guidance) in [
        (
            vec![
                "homeboy",
                "--runner",
                "homeboy-lab",
                "runs",
                "show",
                "run-123",
            ],
            vec!["homeboy runs show run-123", "without --runner"],
        ),
        (
            vec!["homeboy", "--runner", "homeboy-lab", "runs", "list"],
            vec!["homeboy runs list --runner homeboy-lab"],
        ),
    ] {
        let normalized = argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let cli = Cli::parse_from(&normalized);

        let err = route_after_parse_with_provenance(&cli, &normalized, None, None)
            .expect_err("global runner on a runs read is rejected with guidance");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        for expected in guidance {
            assert!(err.message.contains(expected), "{argv:?}: {}", err.message);
        }
    }
}

#[test]
fn cook_coordinators_stay_local_for_all_placements() {
    // Controller coordination stays local while typed child attempts retain
    // the selected Lab runner and its normal placement enforcement (#8519).
    let cook = [
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "homeboy@cook-routing",
        "--verify",
        "cargo test --locked",
    ];
    let batch = [
        "agent-task",
        "fanout",
        "cook-batch",
        "--repo",
        "homeboy",
        "--verify",
        "cargo test --locked agent_task",
        "--run-plan",
        "https://github.com/Extra-Chill/homeboy/issues/7011",
    ];
    for (globals, command, label) in [
        (vec![], cook.as_slice(), None),
        (vec!["--runner", "homeboy-lab"], cook.as_slice(), None),
        (vec!["--placement", "lab"], cook.as_slice(), None),
        (
            vec!["--runner", "homeboy-lab"],
            batch.as_slice(),
            Some("agent-task fanout cook-batch"),
        ),
    ] {
        let mut argv = vec!["homeboy"];
        argv.extend(globals);
        argv.extend(command);
        let cli = Cli::parse_from(&argv);

        let lab_command = lab_offload_command(&cli.command).unwrap().unwrap();

        if let Some(label) = label {
            assert_eq!(lab_command.hot_label, label, "{argv:?}");
        }
        assert!(!lab_command.is_portable(), "{argv:?}");
        assert!(!lab_command.routing_policy.default_lab_offload, "{argv:?}");
    }
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
                auto_placement_reconnect: None,
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
    let _preflight = homeboy::core::parsed_command_preflight::isolate_captured_result_for_test();
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
        auto_placement_reconnect: None,
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
        homeboy_lab_contract::EffectiveExecutionPlacement::Lab
    );
    assert_eq!(
        decision.runner.expect("selected runner").runner_id,
        "admitted-lab"
    );
    homeboy::core::resource_policy_context::reset_captured_context_for_test();
    homeboy::core::parsed_command_preflight::reset_captured_result_for_test();
}

#[test]
fn strip_component_target_replaces_component_selector_with_path() {
    for (args, expected) in [
        (
            vec!["homeboy", "review", "lint", "--fix", "dmc"],
            vec!["homeboy", "review", "lint", "--fix", "--path", "/src/dmc"],
        ),
        (
            vec!["homeboy", "review", "lint", "--fix", "--component", "dmc"],
            vec!["homeboy", "review", "lint", "--fix", "--path", "/src/dmc"],
        ),
        // Passthrough args after `--` are preserved verbatim.
        (
            vec!["homeboy", "lint", "--fix", "dmc", "--", "dmc"],
            vec![
                "homeboy", "lint", "--fix", "--", "dmc", "--path", "/src/dmc",
            ],
        ),
    ] {
        let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();

        assert_eq!(
            strip_component_target_args(&args, "dmc", "/src/dmc"),
            expected,
            "{args:?}"
        );
    }
}

#[test]
fn rewrite_component_target_skips_path_override_or_missing_component() {
    for argv in [
        vec![
            "homeboy",
            "review",
            "lint",
            "--fix",
            "sample-component",
            "--path",
            "/explicit/path",
        ],
        // No component and no --path: source resolves from CWD.
        vec!["homeboy", "review", "lint", "--fix"],
    ] {
        let cli = Cli::parse_from(&argv);
        let normalized = argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();

        assert!(
            rewrite_component_target_to_path(&cli.command, &normalized).is_none(),
            "{argv:?}"
        );
    }
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
fn rewrite_ad_hoc_lab_workspace_inserts_cwd_path_before_passthrough() {
    let dir = tempdir().unwrap();
    let _cwd = CwdGuard::set(dir.path());
    let cwd = std::env::current_dir().expect("current dir");
    let cwd = cwd.to_string_lossy();
    for (argv, expected) in [
        (
            vec!["homeboy", "review", "lint"],
            vec!["homeboy", "review", "lint", "--path", &cwd],
        ),
        (
            vec!["homeboy", "review", "test", "--", "--filter", "ExampleTest"],
            vec![
                "homeboy",
                "review",
                "test",
                "--path",
                &cwd,
                "--",
                "--filter",
                "ExampleTest",
            ],
        ),
    ] {
        let cli = Cli::parse_from(&argv);
        let normalized = argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();

        let rewritten = lab_route_source_path_args(&cli.command, &normalized, false)
            .expect("pathless review command gets an explicit path");

        assert_eq!(rewritten, expected, "{argv:?}");
    }
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
                &lab_command,
                cli.placement,
                lab_routing::captured_pressure_severity().as_deref(),
            ))
        .then(|| runner.to_string())
    })
}

#[test]
fn route_runner_follows_record_ownership_and_explicit_authority() {
    // Controller-local lifecycle reads (and the next actions diagnosis prints
    // for them) resolve against the controller that owns the record, not a
    // connected default Lab runner that never saw it (#11599). Explicit
    // `--placement local` owns the whole lifecycle (#11600), while an explicit
    // runner, explicit Lab placement, or a contract that declares automatic
    // offload still reaches the runner (#9651/#9763).
    let id = OWNER_LOCAL_RUN_ID;
    let lab = connected_default_lab_runner();
    let local_reads = [
        vec!["homeboy", "agent-task", "providers"],
        vec!["homeboy", "agent-task", "status", id],
        vec!["homeboy", "runs", "logs", id],
        vec!["homeboy", "agent-task", "diagnose", id],
        vec!["homeboy", "agent-task", "diagnose", id, "--full"],
        vec!["homeboy", "runs", "evidence", id],
        vec![
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "retry",
            id,
            "--run",
        ],
    ];
    let runner_routed = [
        vec![
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "providers",
        ],
        vec![
            "homeboy",
            "agent-task",
            "providers",
            "--runner",
            "homeboy-lab",
        ],
        vec!["homeboy", "--placement", "lab", "agent-task", "providers"],
        vec!["homeboy", "agent-task", "run-plan", "--plan", "-"],
        vec!["homeboy", "agent-task", "retry", id, "--run"],
    ];
    for args in &local_reads {
        assert_eq!(route_runner_for(args, &lab), None, "{args:?}");
    }
    for args in &runner_routed {
        assert_eq!(
            route_runner_for(args, &lab),
            Some("homeboy-lab".to_string()),
            "{args:?}"
        );
        assert_eq!(route_runner_for(args, &None), None, "{args:?}");
    }
}

#[test]
fn controller_local_record_owns_lifecycle_reads_and_cook_retries_before_lab_selection() {
    crate::test_support::with_isolated_home(|_| {
        let inferred = connected_default_lab_runner();
        let plan = homeboy::agents::agent_task_schedule::AgentTaskPlan::new(
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
            ["homeboy", "runs", "logs", OWNER_LOCAL_RUN_ID].as_slice(),
            ["homeboy", "runs", "evidence", OWNER_LOCAL_RUN_ID].as_slice(),
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

        let executable_retry_args = [
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "retry",
            OWNER_LOCAL_RUN_ID,
            "--run",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let executable_retry =
            Cli::try_parse_from(&executable_retry_args).expect("executable retry parses");
        assert!(
            !controller_owns_agent_task_lifecycle_command(&executable_retry)
                .expect("executable retry owner resolves"),
            "retry --run must reach controller preflight and Lab handoff materialization"
        );

        agent_task_lifecycle::rewrite_record_for_test(OWNER_LOCAL_RUN_ID, |record| {
            record.metadata["cook_id"] = serde_json::json!("cook-owner-routing");
            record.metadata["runner_id"] = serde_json::json!("homeboy-lab");
        })
        .expect("mark the retry as Cook-owned");

        assert!(
            controller_owns_agent_task_lifecycle_command(&executable_retry)
                .expect("Cook retry owner resolves"),
            "a Cook retry must remain on its controller before replaying provider work"
        );
        assert_eq!(
            route_after_parse_with_provenance(
                &executable_retry,
                &executable_retry_args,
                None,
                None,
            )
            .expect("route the generated Cook retry action"),
            None,
            "the routing layer must return a Cook retry to its controller before Lab preflight"
        );
        assert!(
            super::dispatch::reserved_retry_transport_fixture(
                &executable_retry,
                &executable_retry_args,
            )
            .expect("Cook retry handoff decision")
            .is_none(),
            "Cook retries are replayed by their controller lifecycle, not generic Lab routing"
        );
    });
}
