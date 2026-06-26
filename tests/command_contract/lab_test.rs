use super::*;
use crate::cli_surface::{Cli, Commands};
use clap::CommandFactory;
use clap::Parser;

fn parsed_command(args: &[&str]) -> Commands {
    Cli::try_parse_from(args)
        .expect("CLI args should parse")
        .command
}

fn parsed_cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).expect("CLI args should parse")
}

fn supported_lab_command_cases() -> Vec<(Commands, &'static str)> {
    vec![
        (parsed_command(&["homeboy", "lint"]), "lint"),
        (
            parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"]),
            "lint",
        ),
        (parsed_command(&["homeboy", "test"]), "test"),
        (
            parsed_command(&["homeboy", "test", "--changed-since", "origin/main"]),
            "test",
        ),
        (parsed_command(&["homeboy", "audit"]), "audit"),
        (parsed_command(&["homeboy", "review"]), "review"),
        (parsed_command(&["homeboy", "bench"]), "bench"),
        (
            parsed_command(&[
                "homeboy",
                "bench",
                "matrix",
                "--setting-matrix",
                "clients=10,100",
            ]),
            "bench",
        ),
        (
            parsed_command(&["homeboy", "bench", "history", "homeboy"]),
            "bench",
        ),
        (parsed_command(&["homeboy", "fuzz"]), "fuzz"),
        (parsed_command(&["homeboy", "fuzz", "run"]), "fuzz"),
        // `fuzz list` offloads (unlike `bench list`) because fuzz workloads are
        // rig/extension-declared and may only exist on the runner, so the
        // operator must see the runner-resident inventory, not the local one.
        (
            parsed_command(&["homeboy", "fuzz", "list", "--rig", "studio"]),
            "fuzz",
        ),
        (parsed_command(&["homeboy", "trace"]), "trace"),
        (
            parsed_command(&["homeboy", "refactor", "--from", "audit"]),
            "refactor",
        ),
        (
            parsed_command(&["homeboy", "refactor", "--all"]),
            "refactor",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "cook",
                "--to-worktree",
                "homeboy@cook",
                "--verify",
                "true",
                "--prompt",
                "cook",
            ]),
            "agent-task cook/run-plan",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "run-plan", "--plan", "@plan.json"]),
            "agent-task cook/run-plan",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "retry", "agent-task-123", "--run"]),
            "agent-task retry --run",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "run", "agent-task-123"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "run-next"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "status", "agent-task-123"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "logs", "agent-task-123"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "artifacts", "agent-task-123"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "evidence", "agent-task-123"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "review", "agent-task-123"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "list"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "active"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "latest"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "providers"]),
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "fanout",
                "submit-batch",
                "--input",
                "fanout.json",
            ]),
            "agent-task fanout run-plan/submit-batch/status/artifacts",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "fanout",
                "status",
                "fanout-batch-123",
            ]),
            "agent-task fanout run-plan/submit-batch/status/artifacts",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "fanout",
                "artifacts",
                "fanout-batch-123",
            ]),
            "agent-task fanout submit-batch/status/artifacts",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "controller",
                "from-spec",
                "loop.json",
                "--resume",
            ]),
            "agent-task controller from-spec --resume/run-from-spec/materialize/resume",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "controller",
                "run-from-spec",
                "loop.json",
                "--max-actions",
                "1",
            ]),
            "agent-task controller from-spec --resume/run-from-spec/materialize/resume",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "controller",
                "materialize",
                "loop.json",
            ]),
            "agent-task controller from-spec --resume/run-from-spec/materialize/resume",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "controller", "resume", "loop-123"]),
            "agent-task controller from-spec --resume/run-from-spec/materialize/resume",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "auth",
                "status",
                "--secret-env",
                "OPENAI_API_KEY",
            ]),
            "agent-task auth status",
        ),
        (
            parsed_command(&["homeboy", "rig", "check", "studio"]),
            "rig check",
        ),
        (
            parsed_command(&[
                "homeboy",
                "tunnel",
                "preview-consumer",
                "run",
                "--config",
                "preview-consumer.json",
                "--preview-public-url",
                "https://preview.example.test/",
            ]),
            "tunnel preview-consumer run",
        ),
        (
            parsed_command(&[
                "homeboy",
                "tunnel",
                "service",
                "expose",
                "preview",
                "--server",
                "homeboy-lab",
                "--remote-host",
                "127.0.0.1",
                "--remote-port",
                "7331",
                "--auth-mode",
                "ssh-only",
            ]),
            "tunnel service expose",
        ),
        (
            parsed_command(&[
                "homeboy",
                "tunnel",
                "service",
                "start",
                "preview",
                "--cwd",
                "/home/user/Developer/_lab_workspaces/site",
                "--command",
                "npm run dev",
            ]),
            "tunnel service start",
        ),
    ]
}

fn unsupported_lab_command_cases() -> Vec<Commands> {
    vec![
        parsed_command(&["homeboy", "agent-task", "retry", "agent-task-123"]),
        parsed_command(&["homeboy", "agent-task", "loop", "status", "site-loop"]),
        parsed_command(&["homeboy", "review", "--changed-only"]),
        parsed_command(&[
            "homeboy", "refactor", "rename", "--from", "old", "--to", "new",
        ]),
        parsed_command(&["homeboy", "rig", "up", "studio"]),
        parsed_command(&[
            "homeboy", "fleet", "exec", "prod", "--apply", "wp", "plugin", "list",
        ]),
        parsed_command(&["homeboy", "status"]),
        parsed_command(&["homeboy", "bench", "list"]),
    ]
}

#[test]
fn test_lab_runner_supported_labels_are_contract_owned() {
    assert_eq!(
        lab_runner_supported_labels().as_slice(),
        &[
            "agent-task cook/run-plan",
            "agent-task controller from-spec --resume/run-from-spec/materialize/resume",
            "agent-task retry --run",
            "agent-task run/run-next/status/logs/artifacts/evidence/review/list/active/latest/providers",
            "agent-task fanout run-plan/submit-batch/status/artifacts",
            "agent-task auth status",
            "lint",
            "test",
            "audit",
            "review",
            "bench",
            "fuzz",
            "trace",
            "refactor source runs",
            "rig check",
            "tunnel preview-consumer run",
            "tunnel service expose",
            "tunnel service start",
        ]
    );
    for label in lab_runner_supported_labels() {
        assert!(lab_runner_unsupported_message().contains(label));
        assert!(lab_runner_unsupported_hint().contains(label));
    }
}

#[test]
fn rig_check_supports_lab_runner_but_rig_up_stays_local_only() {
    let rig_check = parsed_command(&["homeboy", "rig", "check", "studio"]);
    let rig_check_descriptor = rig_check.descriptor(false);
    assert!(rig_check_descriptor.supports_lab_runner);
    assert!(rig_check_descriptor.lab_runner_unsupported_reason.is_none());

    let rig_up = parsed_command(&["homeboy", "rig", "up", "studio"]);
    let rig_up_descriptor = rig_up.descriptor(false);
    assert!(!rig_up_descriptor.supports_lab_runner);
    assert!(rig_up_descriptor
        .lab_runner_unsupported_reason
        .is_some_and(|reason| reason.contains("rig up")));
}

#[test]
fn lab_route_contract_carries_command_specific_requirements() {
    let command = parsed_command(&["homeboy", "trace"]);

    let route_contract = command
        .lab_route_contract()
        .expect("route contract resolves")
        .expect("trace has a Lab route contract");

    assert_eq!(route_contract.command.hot_label, "trace");
    assert!(route_contract.requires_playwright);
    assert!(route_contract.required_extensions.is_empty());
    assert!(
        !route_contract
            .command
            .routing_policy
            .infer_source_path_tools
    );
}

#[test]
fn local_execution_policy_names_legacy_flag_combinations() {
    let default_policy = LabLocalExecutionPolicy::default();
    assert!(!default_policy.allow_local_hot());
    assert!(!default_policy.allow_local_fallback());
    assert!(!default_policy.deny_local_execution());

    let permissive_policy = LabLocalExecutionPolicy::from_flags(true, true, false);
    assert!(permissive_policy.allow_local_hot());
    assert!(permissive_policy.allow_local_fallback());
    assert!(!permissive_policy.deny_local_execution());

    let lab_only_policy = LabLocalExecutionPolicy::from_flags(true, true, true);
    assert!(!lab_only_policy.allow_local_hot());
    assert!(!lab_only_policy.allow_local_fallback());
    assert!(lab_only_policy.deny_local_execution());
}

#[test]
fn test_supports_lab_runner() {
    for (command, _) in supported_lab_command_cases() {
        assert!(command.supports_lab_runner());
    }
    for command in unsupported_lab_command_cases() {
        assert!(!command.supports_lab_runner());
    }

    let cli = parsed_cli(&[
        "homeboy",
        "agent-task",
        "run",
        "--runner",
        "homeboy-lab",
        "agent-task-123",
    ]);
    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert!(cli.command.supports_lab_runner());
    let cli = parsed_cli(&["homeboy", "lint", "--runner", "lab-a"]);
    assert_eq!(cli.runner.as_deref(), Some("lab-a"));
    assert!(cli.command.supports_lab_runner());

    let cli = parsed_cli(&[
        "homeboy",
        "trace",
        "--runner",
        "homeboy-lab",
        "--allow-local-fallback",
    ]);
    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert!(cli.allow_local_fallback);

    let cli = parsed_cli(&["homeboy", "--force-hot", "--allow-local-hot", "bench"]);
    assert!(cli.force_hot);
    assert!(cli.allow_local_hot);
    assert!(cli.command.supports_lab_runner());
}

#[test]
fn fuzz_run_and_list_offload_but_other_subcommands_stay_local() {
    // `fuzz run` and `fuzz list` select the configured Lab runner so both the
    // workload execution and the runner-resident workload inventory route to
    // the runner. This mirrors how `bench run`/discovery offload while keeping
    // the read-only/local-only fuzz subcommands (contract, plan, validate,
    // report, compare, replay, inspect) local.
    for args in [
        ["homeboy", "fuzz", "run"].as_slice(),
        ["homeboy", "fuzz", "list"].as_slice(),
        ["homeboy", "fuzz", "list", "--rig", "studio"].as_slice(),
    ] {
        let command = parsed_command(args);
        assert!(
            command.supports_lab_runner(),
            "expected {args:?} to support Lab offload"
        );
        let contract = command.lab_contract().expect("fuzz offload contract");
        assert_eq!(contract.hot_label, "fuzz");
        assert_eq!(contract.portability, LabCommandPortability::Portable);
    }

    for args in [
        ["homeboy", "fuzz", "contract"].as_slice(),
        ["homeboy", "fuzz", "plan"].as_slice(),
        ["homeboy", "fuzz", "validate", "campaign.json"].as_slice(),
        ["homeboy", "fuzz", "inspect", "run-123"].as_slice(),
    ] {
        let command = parsed_command(args);
        assert!(
            !command.supports_lab_runner(),
            "expected {args:?} to stay local"
        );
        assert!(command.lab_contract().is_none());
    }
}

#[test]
fn test_lab_command_contracts_cover_hot_commands() {
    for (command, _) in supported_lab_command_cases() {
        let contract = command.lab_contract().expect("hot contract");
        assert!(
            lab_runner_summary_covers_contract_label(contract.hot_label),
            "Lab support summary omitted `{}`",
            contract.hot_label
        );
        assert_eq!(contract.portability, LabCommandPortability::Portable);
    }

    let trace = parsed_command(&["homeboy", "trace"])
        .lab_contract()
        .expect("trace contract");
    assert!(lab_runner_summary_covers_contract_label(trace.hot_label));
    assert_eq!(trace.extra_required_tools, LAB_TRACE_EXTRA_TOOLS);
    assert!(!trace.routing_policy.requires_extension_parity);
    assert!(!trace.routing_policy.infer_source_path_tools);
    assert_eq!(
        trace.workspace_mode_policy,
        LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot
    );

    let trace_compare_refs = parsed_command(&[
        "homeboy",
        "trace",
        "compare",
        "woocommerce-gateway-stripe",
        "ece-product-page-waterfall",
        "--baseline-target",
        "origin/develop",
        "--candidate",
        "32f68bb07ac0efa1d754f78e2adc8de115ddca6f",
    ])
    .lab_contract()
    .expect("trace compare contract");
    assert_eq!(
        trace_compare_refs.workspace_mode_policy,
        LabWorkspaceModePolicy::Git
    );

    let lint = parsed_command(&["homeboy", "lint"])
        .lab_contract()
        .expect("lint contract");
    assert!(lint.routing_policy.requires_extension_parity);
    assert!(lint.routing_policy.infer_source_path_tools);
    assert!(lint.routing_policy.release_gate);

    let test_full = parsed_command(&["homeboy", "test"])
        .lab_contract()
        .expect("test contract");
    assert!(test_full.routing_policy.release_gate);

    let audit_full = parsed_command(&["homeboy", "audit"])
        .lab_contract()
        .expect("audit contract");
    assert!(audit_full.routing_policy.release_gate);

    let review_full = parsed_command(&["homeboy", "review"])
        .lab_contract()
        .expect("review contract");
    assert!(review_full.routing_policy.release_gate);

    assert!(
        parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"])
            .lab_contract()
            .expect("changed-scope lint contract")
            .routing_policy
            .release_gate
    );

    // Non-gate commands are NOT release gates.
    assert!(
        !parsed_command(&["homeboy", "bench"])
            .lab_contract()
            .expect("bench contract")
            .routing_policy
            .release_gate
    );
    assert!(
        !parsed_command(&["homeboy", "trace"])
            .lab_contract()
            .expect("trace contract")
            .routing_policy
            .release_gate
    );
    assert!(
        !parsed_command(&[
            "homeboy",
            "agent-task",
            "cook",
            "--to-worktree",
            "homeboy@smoke",
            "--verify",
            "true",
            "--prompt",
            "cook",
        ])
        .lab_contract()
        .expect("agent-task contract")
        .routing_policy
        .release_gate
    );

    for args in [
        ["homeboy", "agent-task", "run", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "run-next"].as_slice(),
        ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "logs", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "artifacts", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "review", "agent-task-123"].as_slice(),
        [
            "homeboy",
            "agent-task",
            "fanout",
            "status",
            "fanout-batch-123",
        ]
        .as_slice(),
        [
            "homeboy",
            "agent-task",
            "fanout",
            "artifacts",
            "fanout-batch-123",
        ]
        .as_slice(),
        ["homeboy", "agent-task", "controller", "resume", "loop-123"].as_slice(),
    ] {
        let contract = parsed_command(args)
            .lab_contract()
            .expect("runner-backed agent-task inspection contract");
        assert_eq!(contract.source_path_mode, LabSourcePathMode::RunnerResident);
        assert_eq!(
            contract.workspace_mode_policy,
            LabWorkspaceModePolicy::RunnerResident
        );
        assert!(!contract.routing_policy.default_lab_offload);
    }

    let fanout_submit_batch = parsed_command(&[
        "homeboy",
        "agent-task",
        "fanout",
        "submit-batch",
        "--input",
        "fanout.json",
    ])
    .lab_contract()
    .expect("fanout submit-batch contract");
    assert_eq!(
        fanout_submit_batch.hot_label,
        "agent-task fanout submit-batch"
    );
    assert_eq!(
        fanout_submit_batch.source_path_mode,
        LabSourcePathMode::CwdOrPathFlag
    );
    assert_eq!(
        fanout_submit_batch.workspace_mode_policy,
        LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot
    );
    assert!(!fanout_submit_batch.routing_policy.default_lab_offload);
    assert!(!fanout_submit_batch.routing_policy.requires_extension_parity);
    assert!(!fanout_submit_batch.routing_policy.infer_source_path_tools);

    assert!(
        parsed_command(&[
            "homeboy",
            "agent-task",
            "controller",
            "from-spec",
            "loop.json",
        ])
        .lab_contract()
        .is_none(),
        "from-spec without --resume only writes local controller state"
    );

    let auth_status = parsed_command(&[
        "homeboy",
        "agent-task",
        "auth",
        "status",
        "--secret-env",
        "OPENAI_API_KEY",
    ])
    .lab_contract()
    .expect("agent-task auth status contract");
    assert!(!auth_status.routing_policy.default_lab_offload);
    assert!(!auth_status.routing_policy.requires_extension_parity);
    assert!(!auth_status.routing_policy.infer_source_path_tools);

    let tunnel_preview_consumer_run = parsed_command(&[
        "homeboy",
        "tunnel",
        "preview-consumer",
        "run",
        "--config",
        "preview-consumer.json",
        "--preview-public-url",
        "https://preview.example.test/",
    ])
    .lab_contract()
    .expect("tunnel preview-consumer run contract");
    assert!(lab_runner_summary_covers_contract_label(
        tunnel_preview_consumer_run.hot_label
    ));

    let tunnel_service_start = parsed_command(&[
        "homeboy",
        "tunnel",
        "service",
        "start",
        "preview",
        "--cwd",
        "/home/user/Developer/_lab_workspaces/site",
        "--command",
        "npm run dev",
    ])
    .lab_contract()
    .expect("tunnel service start contract");
    assert!(lab_runner_summary_covers_contract_label(
        tunnel_service_start.hot_label
    ));
    assert_eq!(
        tunnel_service_start.source_path_mode,
        LabSourcePathMode::RunnerResident
    );
    assert_eq!(
        tunnel_service_start.workspace_mode_policy,
        LabWorkspaceModePolicy::RunnerResident
    );
    assert!(!tunnel_service_start.routing_policy.default_lab_offload);
    assert!(!tunnel_service_start.routing_policy.infer_source_path_tools);

    let tunnel_service_expose = parsed_command(&[
        "homeboy",
        "tunnel",
        "service",
        "expose",
        "preview",
        "--server",
        "homeboy-lab",
        "--remote-host",
        "127.0.0.1",
        "--remote-port",
        "7331",
        "--auth-mode",
        "ssh-only",
    ])
    .lab_contract()
    .expect("tunnel service expose contract");
    assert!(lab_runner_summary_covers_contract_label(
        tunnel_service_expose.hot_label
    ));
    assert_eq!(
        tunnel_service_expose.source_path_mode,
        LabSourcePathMode::RunnerResident
    );
    assert_eq!(
        tunnel_service_expose.workspace_mode_policy,
        LabWorkspaceModePolicy::RunnerResident
    );
    assert!(!tunnel_service_expose.routing_policy.default_lab_offload);
    assert!(!tunnel_service_expose.routing_policy.infer_source_path_tools);

    let rig = parsed_command(&["homeboy", "rig", "up", "studio"])
        .lab_contract()
        .expect("rig up contract");
    assert_eq!(rig.hot_label, "rig up");
    assert!(matches!(
        rig.portability,
        LabCommandPortability::LocalOnly(reason) if reason.contains("single-workspace Lab snapshot")
    ));

    let fleet = parsed_command(&[
        "homeboy", "fleet", "exec", "prod", "--apply", "wp", "plugin", "list",
    ])
    .lab_contract()
    .expect("fleet exec contract");
    assert_eq!(fleet.hot_label, "fleet exec");
    assert!(matches!(
        fleet.portability,
        LabCommandPortability::LocalOnly(reason) if reason.contains("config parity")
    ));

    for args in [
        ["homeboy", "audit", "--changed-since", "origin/main"].as_slice(),
        ["homeboy", "review", "--changed-since", "origin/main"].as_slice(),
        ["homeboy", "review", "--changed-only"].as_slice(),
    ] {
        parsed_command(args)
            .lab_contract()
            .expect("scoped hot command should have a Lab plan contract");
    }

    for args in [
        ["homeboy", "lint", "--changed-since", "origin/main"].as_slice(),
        ["homeboy", "lint", "--changed-only"].as_slice(),
        ["homeboy", "test", "--changed-since", "origin/main"].as_slice(),
    ] {
        let contract = parsed_command(args)
            .lab_contract()
            .expect("scoped hot command should have a Lab plan contract");
        assert!(matches!(
            contract.portability,
            LabCommandPortability::Portable
        ));
    }

    assert!(parsed_command(&["homeboy", "status"])
        .lab_contract()
        .is_none());
    assert!(parsed_command(&["homeboy", "bench", "list"])
        .lab_contract()
        .is_none());
    assert!(parsed_command(&["homeboy", "audit", "--conventions"])
        .lab_contract()
        .is_none());
    assert!(
        parsed_command(&["homeboy", "agent-task", "loop", "resume", "loop-123"])
            .lab_contract()
            .is_none()
    );

    assert!(parsed_command(&[
        "homeboy",
        "agent-task",
        "auth",
        "map-env",
        "OPENAI_API_KEY",
        "--from",
        "OPENAI_SOURCE_API_KEY",
    ])
    .lab_contract()
    .is_none());
    assert!(
        parsed_command(&["homeboy", "lint", "--file", "src/main.rs"])
            .lab_contract()
            .is_none()
    );
}

#[test]
fn command_portability_contract_exposes_delegated_lab_descriptors() {
    for args in [
        ["homeboy", "bench"].as_slice(),
        ["homeboy", "lint"].as_slice(),
        ["homeboy", "trace"].as_slice(),
        ["homeboy", "rig", "check", "studio"].as_slice(),
        [
            "homeboy",
            "tunnel",
            "preview-consumer",
            "run",
            "--config",
            "preview-consumer.json",
            "--preview-public-url",
            "https://preview.example.test/",
        ]
        .as_slice(),
    ] {
        let command = parsed_command(args);
        assert_eq!(
            command.portability_contract().lab_command(),
            command.lab_contract(),
            "portability descriptor diverged for {args:?}"
        );
    }

    assert!(parsed_command(&["homeboy", "bench", "list"])
        .portability_contract()
        .lab_command()
        .is_none());
}

#[test]
fn agent_task_git_checkout_policy_uses_default_backend_when_backend_is_omitted() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "cook",
        "--to-worktree",
        "homeboy@smoke",
        "--cwd",
        "/work/repo",
        "--prompt",
        "cook",
    ]);
    let Commands::AgentTask(args) = command else {
        panic!("expected agent-task command");
    };

    assert!(agent_task_provider_requires_cwd_git_checkout_with(
        &args.command,
        || Some("default-patch-provider".to_string()),
        |backend, selector| backend == "default-patch-provider" && selector.is_none(),
    ));
}

#[test]
fn agent_task_git_checkout_policy_keeps_non_cwd_dispatch_snapshot_eligible() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "cook",
        "--to-worktree",
        "homeboy@smoke",
        "--verify",
        "true",
        "--prompt",
        "cook",
    ]);
    let Commands::AgentTask(args) = command else {
        panic!("expected agent-task command");
    };

    assert!(!agent_task_provider_requires_cwd_git_checkout_with(
        &args.command,
        || Some("default-patch-provider".to_string()),
        |backend, _| backend == "default-patch-provider",
    ));
}

#[test]
fn agent_task_git_checkout_policy_treats_workspace_like_cwd() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "cook",
        "--to-worktree",
        "homeboy@smoke",
        "--verify",
        "true",
        "--workspace",
        "/work/repo",
        "--prompt",
        "cook",
    ]);
    let Commands::AgentTask(args) = command else {
        panic!("expected agent-task command");
    };

    assert!(agent_task_provider_requires_cwd_git_checkout_with(
        &args.command,
        || Some("default-patch-provider".to_string()),
        |backend, selector| backend == "default-patch-provider" && selector.is_none(),
    ));
}

#[test]
fn agent_task_loop_status_is_durable_controller_surface_not_cook_dispatch() {
    let command = parsed_command(&["homeboy", "agent-task", "loop", "status", "site-loop"]);
    let Commands::AgentTask(args) = command else {
        panic!("expected agent-task command");
    };

    assert!(!agent_task_provider_requires_cwd_git_checkout_with(
        &args.command,
        || Some("default-patch-provider".to_string()),
        |backend, _| backend == "default-patch-provider",
    ));
    assert!(
        parsed_command(&["homeboy", "agent-task", "loop", "status", "site-loop"])
            .lab_contract()
            .is_none()
    );
}

#[test]
fn agent_task_git_checkout_policy_covers_cook_dispatch_workspace() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "cook",
        "--to-worktree",
        "homeboy@smoke",
        "--verify",
        "true",
        "--cwd",
        "/work/repo",
        "--backend",
        "generic-patch-provider",
        "--selector",
        "selected",
        "--prompt",
        "cook",
    ]);
    let Commands::AgentTask(ref args) = command else {
        panic!("expected agent-task command");
    };

    assert!(agent_task_provider_requires_cwd_git_checkout_with(
        &args.command,
        || Some("default-patch-provider".to_string()),
        |backend, _| backend == "default-patch-provider",
    ));
    assert_eq!(
        parsed_command(&[
            "homeboy",
            "agent-task",
            "cook",
            "--to-worktree",
            "homeboy@smoke",
            "--verify",
            "true",
            "--cwd",
            "/work/repo",
            "--backend",
            "generic-patch-provider",
            "--selector",
            "selected",
            "--prompt",
            "cook",
        ])
        .lab_contract()
        .expect("agent-task cook contract")
        .workspace_mode_policy,
        LabWorkspaceModePolicy::GitCheckoutRequired
    );
}

#[test]
fn agent_task_git_checkout_policy_covers_controller_from_spec_resume_backend() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "controller",
        "from-spec",
        "loop.json",
        "--resume",
        "--dispatch-backend",
        "patch-provider",
        "--dispatch-selector",
        "selected",
    ]);
    let Commands::AgentTask(args) = command else {
        panic!("expected agent-task command");
    };

    assert!(agent_task_provider_requires_cwd_git_checkout_with(
        &args.command,
        || None,
        |backend, selector| backend == "patch-provider" && selector == Some("selected"),
    ));
}

#[test]
fn agent_task_git_checkout_policy_covers_controller_run_from_spec_backend() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "controller",
        "run-from-spec",
        "loop.json",
        "--max-actions",
        "1",
        "--dispatch-backend",
        "patch-provider",
        "--dispatch-selector",
        "selected",
    ]);
    let Commands::AgentTask(args) = command else {
        panic!("expected agent-task command");
    };

    assert!(agent_task_provider_requires_cwd_git_checkout_with(
        &args.command,
        || None,
        |backend, selector| backend == "patch-provider" && selector == Some("selected"),
    ));
}

#[test]
fn agent_task_git_checkout_policy_requires_git_for_explicit_controller_backend() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "controller",
        "from-spec",
        "loop.json",
        "--resume",
        "--dispatch-backend",
        "extension-provider",
    ]);
    let Commands::AgentTask(ref args) = command else {
        panic!("expected agent-task command");
    };

    assert!(agent_task_provider_requires_cwd_git_checkout_with(
        &args.command,
        || None,
        |_, _| false,
    ));
    assert_eq!(
        command
            .lab_contract()
            .expect("agent-task controller contract")
            .workspace_mode_policy,
        LabWorkspaceModePolicy::GitCheckoutRequired
    );
}

#[test]
fn agent_task_git_checkout_policy_skips_controller_materialize() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "controller",
        "materialize",
        "loop.json",
    ]);
    let Commands::AgentTask(args) = command else {
        panic!("expected agent-task command");
    };

    assert!(!agent_task_provider_requires_cwd_git_checkout_with(
        &args.command,
        || Some("patch-provider".to_string()),
        |backend, _| backend == "patch-provider",
    ));
}

#[test]
fn test_lab_runner_unsupported_hot_command_reasons() {
    assert!(parsed_command(&["homeboy", "rig", "up", "studio"])
        .lab_runner_unsupported_reason()
        .expect("rig up reason")
        .contains("single-workspace Lab snapshot"));
    assert!(parsed_command(&[
        "homeboy", "fleet", "exec", "prod", "--apply", "wp", "plugin", "list",
    ])
    .lab_runner_unsupported_reason()
    .expect("fleet exec reason")
    .contains("config parity"));
    assert!(
        parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"])
            .lab_runner_unsupported_reason()
            .is_none()
    );
    assert!(
        parsed_command(&["homeboy", "test", "--changed-since", "origin/main"])
            .lab_runner_unsupported_reason()
            .is_none()
    );
    assert!(parsed_command(&["homeboy", "status"])
        .lab_runner_unsupported_reason()
        .is_none());
}

#[test]
fn test_lab_runner_flag_is_visible_in_help() {
    let root_help = Cli::command()
        .try_get_matches_from(["homeboy", "--help"])
        .expect_err("help exits")
        .to_string();
    assert!(root_help.contains("--runner"));

    for args in [
        ["homeboy", "rig", "check", "--help"].as_slice(),
        ["homeboy", "build", "--help"].as_slice(),
        ["homeboy", "bench", "list", "--help"].as_slice(),
    ] {
        let help = Cli::command()
            .try_get_matches_from(args)
            .expect_err("help exits")
            .to_string();
        assert!(help.contains("--runner"), "{args:?} help omitted --runner");
    }
}

#[test]
fn test_lab_offload_mutation_flag() {
    assert_eq!(
        parsed_command(&["homeboy", "lint", "--fix"]).lab_offload_mutation_flag(),
        Some("--fix")
    );
    assert_eq!(
        parsed_command(&["homeboy", "test", "--write"]).lab_offload_mutation_flag(),
        Some("--write")
    );
    assert_eq!(
        parsed_command(&["homeboy", "bench", "--baseline"]).lab_offload_mutation_flag(),
        Some("--baseline/--ratchet")
    );
    assert_eq!(
        parsed_command(&["homeboy", "trace", "--keep-overlay"]).lab_offload_mutation_flag(),
        Some("--keep-overlay")
    );
    assert_eq!(
        parsed_command(&["homeboy", "refactor", "--from", "audit", "--write"])
            .lab_offload_mutation_flag(),
        Some("--write/--commit")
    );
    assert_eq!(
        parsed_command(&["homeboy", "audit"]).lab_offload_mutation_flag(),
        None
    );
    assert_eq!(
        parsed_command(&["homeboy", "audit", "--baseline"]).lab_offload_mutation_flag(),
        Some("--baseline/--ratchet")
    );
    assert_eq!(
        parsed_command(&["homeboy", "audit", "--ratchet"]).lab_offload_mutation_flag(),
        Some("--baseline/--ratchet")
    );
}

#[test]
fn lab_mutation_patch_capture_is_descriptor_owned() {
    let mutating_lint = parsed_command(&["homeboy", "lint", "--fix"]);
    let mutating_descriptor = mutating_lint.descriptor(false);
    assert!(mutating_lint.lab_offload_captures_mutation_patch());
    assert!(mutating_descriptor.lab_offload_captures_mutation_patch);
    assert_eq!(mutating_descriptor.lab_offload_mutation_flag, Some("--fix"));

    let read_only_lint = parsed_command(&["homeboy", "lint"]);
    let read_only_descriptor = read_only_lint.descriptor(false);
    assert!(!read_only_lint.lab_offload_captures_mutation_patch());
    assert!(!read_only_descriptor.lab_offload_captures_mutation_patch);
    assert_eq!(read_only_descriptor.lab_offload_mutation_flag, None);
}
