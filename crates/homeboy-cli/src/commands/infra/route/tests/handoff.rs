#![cfg(test)]

use super::*;
use clap::Parser;
use homeboy::command_contract::{lab_runner_supports_contract_label, LabCommandPortability};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use tempfile::tempdir;

use super::*;

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
fn linked_local_rig_check_disables_default_lab_offload() {
    let temp_home = tempdir().expect("temp home");
    let _home = EnvGuard::set("HOME", temp_home.path().to_str().expect("home path"));
    write_rig_source_metadata(temp_home.path(), "linked-local", true);
    let cli = Cli::parse_from(["homeboy", "rig", "check", "linked-local"]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "rig check");
    assert!(command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.infer_source_path_tools);
    assert!(cli.command.supports_lab_runner());
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

    let outcome = route_after_parse(&cli, &normalized, None)
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
fn extension_update_requires_explicit_lab_runner_without_extension_parity() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "lab",
        "extension",
        "update",
        "wordpress",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "extension update");
    assert!(command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.requires_extension_parity);
    assert!(command.required_extensions.is_empty());
    assert!(!command.routing_policy.infer_source_path_tools);
    assert!(cli.command.supports_lab_runner());
}

#[test]
fn extension_refresh_requires_explicit_lab_runner_without_extension_parity() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "lab",
        "extension",
        "refresh",
        "https://github.com/Extra-Chill/homeboy-extensions.git",
        "--id",
        "wordpress",
        "--ref",
        "6ff93f43",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "extension refresh");
    assert!(command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.requires_extension_parity);
    assert!(command.required_extensions.is_empty());
    assert!(!command.routing_policy.infer_source_path_tools);
    assert!(cli.command.supports_lab_runner());
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

    let outcome = route_after_parse(&cli, &normalized, None)
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

    let outcome = route_after_parse(&cli, &normalized, None)
        .expect("dev-run should execute its own runner lifecycle");

    assert_eq!(outcome, None);
    assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
}

#[test]
fn extension_show_routes_to_explicit_lab_runner() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "lab",
        "extension",
        "show",
        "wordpress",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "extension show");
    assert!(command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.requires_extension_parity);
    assert!(command.required_extensions.is_empty());
    assert!(!command.routing_policy.infer_source_path_tools);
    assert!(cli.command.supports_lab_runner());
}

#[test]
fn fuzz_doctor_supports_runner_lab_placement_route() {
    let cli = Cli::parse_from([
        "homeboy",
        "fuzz",
        "doctor",
        "--extension",
        "nodejs",
        "--runner",
        "homeboy-lab",
        "--placement",
        "lab",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();
    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert_eq!(cli.placement, crate::cli_surface::Placement::Lab);
    assert_eq!(command.hot_label, "fuzz doctor");
    assert!(lab_runner_supports_contract_label(command.hot_label));
    assert!(command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
    assert!(command.routing_policy.requires_extension_parity);
    assert!(command.routing_policy.read_only_polling);
    assert_eq!(command.required_extensions, vec!["nodejs".to_string()]);
    assert_eq!(
        command.source_path_mode,
        runners::LabOffloadSourcePathMode::RunnerResident
    );
    assert_eq!(
        command.workspace_mode_policy,
        runners::LabOffloadWorkspaceModePolicy::RunnerResident
    );
    assert!(cli.command.lab_offload_mutation_flag().is_none());
    assert!(!cli.command.lab_offload_captures_mutation_patch());
    assert!(cli.command.supports_lab_runner());
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

    let outcome = route_after_parse(&cli, &normalized, None)
        .expect("fuzz doctor without --runner should remain a local diagnostic");

    assert_eq!(outcome, None);
    assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
}

#[test]
fn extension_list_stays_local_only() {
    let cli = Cli::parse_from(["homeboy", "--runner", "lab", "extension", "list"]);

    assert!(lab_offload_command(&cli.command).unwrap().is_none());
    assert!(!cli.command.supports_lab_runner());
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

    let err = route_after_parse(
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

        let outcome = route_after_parse(&cli, &normalized, None)
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

    let err = route_after_parse(&cli, &normalized, None)
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

        let outcome = route_after_parse(&cli, &normalized, None)
            .expect("runs artifact attach command-local runner option should not be rejected");

        assert_eq!(outcome, None);
    }
}

#[test]
fn agent_task_inspection_commands_support_runner_resident_recovery() {
    for args in [
        ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "logs", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "artifacts", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "review", "agent-task-123"].as_slice(),
    ] {
        let cli = Cli::parse_from(args);
        let command = lab_offload_command(&cli.command).unwrap().unwrap();
        assert!(lab_runner_supports_contract_label(command.hot_label));
        assert_eq!(
            command.source_path_mode,
            runners::LabOffloadSourcePathMode::RunnerResident
        );
        assert_eq!(
            command.workspace_mode_policy,
            runners::LabOffloadWorkspaceModePolicy::RunnerResident
        );
        assert!(!command.routing_policy.default_lab_offload);
    }
}

#[test]
fn agent_task_retry_run_supports_explicit_runner() {
    for args in [
        [
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "retry",
            "agent-task-123",
            "--run",
        ],
        [
            "homeboy",
            "agent-task",
            "retry",
            "agent-task-123",
            "--run",
            "--runner",
            "homeboy-lab",
        ],
    ] {
        let cli = Cli::parse_from(args);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(lab_runner_supports_contract_label(command.hot_label));
        assert!(command.is_portable());
        assert!(command.routing_policy.default_lab_offload);
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
fn agent_task_providers_supports_explicit_runner_discovery() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "agent-task",
        "providers",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert!(lab_runner_supports_contract_label(command.hot_label));
    assert!(command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.requires_extension_parity);
    assert!(command.required_extensions.is_empty());
    assert!(!command.routing_policy.infer_source_path_tools);
}

#[test]
fn agent_task_controller_run_from_spec_supports_lab_placement_runner_routing() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "--placement",
        "lab",
        "agent-task",
        "controller",
        "run-from-spec",
        "loop.json",
        "--max-actions",
        "1",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();
    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert_eq!(cli.placement, crate::cli_surface::Placement::Lab);
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

    let command = lab_offload_command(&cli.command).unwrap().unwrap();
    assert_eq!(command.hot_label, "agent-task fanout submit-batch");
    assert!(!command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.infer_source_path_tools);

    let err = route_after_parse(&cli, &normalized, None)
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
fn agent_task_fanout_run_plan_coordination_is_controller_local() {
    let normalized = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "--placement".to_string(),
        "lab".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "run-plan".to_string(),
        "--input".to_string(),
        "fanout.json".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    // Fanout coordination is controller-owned so durable batch state,
    // worktree ownership, and recovery stay local; the coordinator itself
    // is not Lab-portable, though the independent cooks it generates may use
    // their own Lab placement (#8045).
    let command = lab_offload_command(&cli.command).unwrap().unwrap();
    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert_eq!(cli.placement, crate::cli_surface::Placement::Lab);
    assert_eq!(command.hot_label, "agent-task fanout run-plan");
    assert!(!command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
    assert!(!command.routing_policy.requires_extension_parity);
    assert!(cli
        .command
        .lab_runner_unsupported_reason()
        .is_some_and(|reason| reason.contains("controller-owned")));
}

#[test]
fn agent_task_fanout_cook_batch_run_plan_keeps_cook_coordinators_local() {
    let normalized = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "--placement".to_string(),
        "lab".to_string(),
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
    assert_eq!(cli.placement, crate::cli_surface::Placement::Lab);
    assert_eq!(command.hot_label, "agent-task fanout cook-batch");
    assert!(!command.is_portable());
    assert!(!command.routing_policy.default_lab_offload);
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
fn lab_command_preserves_local_only_contract_shape() {
    let cli = Cli::parse_from(["homeboy", "rig", "up", "demo"]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "rig up");
    assert!(matches!(
        command.portability,
        LabCommandPortability::LocalOnly(_)
    ));
    assert!(!command.routing_policy.requires_extension_parity);
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
