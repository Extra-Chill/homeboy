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

#[test]
fn test_lab_runner_supported_labels_are_contract_owned() {
    assert_eq!(
        lab_runner_supported_labels().as_slice(),
        &[
            "agent-task dispatch/cook/loop/run-plan",
            "agent-task controller from-spec --resume/materialize/resume",
            "agent-task retry --run",
            "agent-task status/logs/artifacts/review/providers",
            "agent-task auth status",
            "lint",
            "test",
            "audit",
            "bench",
            "trace",
            "refactor source runs",
            "rig check",
            "tunnel preview-consumer run",
            "tunnel service expose",
            "tunnel service start",
        ]
    );
    assert_eq!(
        lab_runner_unsupported_message(),
        "--runner is only supported for commands with portable Lab offload support: agent-task dispatch/cook/loop/run-plan, agent-task controller from-spec --resume/materialize/resume, agent-task retry --run, agent-task status/logs/artifacts/review/providers, agent-task auth status, lint, test, audit, bench, trace, refactor source runs, rig check, tunnel preview-consumer run, tunnel service expose, and tunnel service start"
    );
    assert_eq!(
        lab_runner_unsupported_hint(),
        "Current Lab offload support: agent-task dispatch/cook/loop/run-plan, agent-task controller from-spec --resume/materialize/resume, agent-task retry --run, agent-task status/logs/artifacts/review/providers, agent-task auth status, full lint, full test, audit, bench run, trace, refactor source runs, rig check, tunnel preview-consumer run, tunnel service expose, and tunnel service start."
    );
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
fn test_supports_lab_runner() {
    assert!(parsed_command(&["homeboy", "lint"]).supports_lab_runner());
    assert!(parsed_command(&["homeboy", "test"]).supports_lab_runner());
    assert!(parsed_command(&["homeboy", "audit"]).supports_lab_runner());
    assert!(parsed_command(&["homeboy", "refactor", "--from", "audit"]).supports_lab_runner());
    assert!(parsed_command(&["homeboy", "refactor", "--all"]).supports_lab_runner());
    assert!(parsed_command(&["homeboy", "bench"]).supports_lab_runner());
    assert!(parsed_command(&[
        "homeboy",
        "bench",
        "matrix",
        "--setting-matrix",
        "clients=10,100"
    ])
    .supports_lab_runner());
    assert!(parsed_command(&["homeboy", "bench", "history", "homeboy"]).supports_lab_runner());
    assert!(parsed_command(&["homeboy", "trace"]).supports_lab_runner());
    assert!(
        parsed_command(&["homeboy", "agent-task", "dispatch", "--prompt", "cook"])
            .supports_lab_runner()
    );
    assert!(
        parsed_command(&["homeboy", "agent-task", "run-plan", "--plan", "@plan.json"])
            .supports_lab_runner()
    );
    assert!(
        parsed_command(&["homeboy", "agent-task", "retry", "agent-task-123", "--run"])
            .supports_lab_runner()
    );
    assert!(
        !parsed_command(&["homeboy", "agent-task", "retry", "agent-task-123"])
            .supports_lab_runner()
    );
    assert!(
        parsed_command(&["homeboy", "agent-task", "status", "agent-task-123"])
            .supports_lab_runner()
    );
    assert!(
        parsed_command(&["homeboy", "agent-task", "logs", "agent-task-123"]).supports_lab_runner()
    );
    assert!(
        parsed_command(&["homeboy", "agent-task", "artifacts", "agent-task-123"])
            .supports_lab_runner()
    );
    assert!(
        parsed_command(&["homeboy", "agent-task", "review", "agent-task-123"])
            .supports_lab_runner()
    );
    assert!(parsed_command(&["homeboy", "agent-task", "providers"]).supports_lab_runner());
    assert!(parsed_command(&[
        "homeboy",
        "tunnel",
        "preview-consumer",
        "run",
        "--config",
        "preview-consumer.json",
        "--preview-public-url",
        "https://preview.example.test/"
    ])
    .supports_lab_runner());
    assert!(parsed_command(&[
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
    .supports_lab_runner());
    assert!(parsed_command(&[
        "homeboy",
        "agent-task",
        "auth",
        "status",
        "--secret-env",
        "OPENAI_API_KEY",
    ])
    .supports_lab_runner());
    assert!(parsed_command(&[
        "homeboy",
        "agent-task",
        "loop",
        "--to-worktree",
        "homeboy@smoke",
        "--verify",
        "true",
        "--prompt",
        "cook"
    ])
    .supports_lab_runner());
    assert!(
        !parsed_command(&["homeboy", "refactor", "rename", "--from", "old", "--to", "new",])
            .supports_lab_runner()
    );
    assert!(!parsed_command(&["homeboy", "rig", "up", "studio"]).supports_lab_runner());
    assert!(!parsed_command(&[
        "homeboy", "fleet", "exec", "prod", "--apply", "wp", "plugin", "list",
    ])
    .supports_lab_runner());
    assert!(!parsed_command(&["homeboy", "status"]).supports_lab_runner());
    assert!(!parsed_command(&["homeboy", "bench", "list"]).supports_lab_runner());
    assert!(
        !parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"])
            .supports_lab_runner()
    );
    assert!(
        !parsed_command(&["homeboy", "test", "--changed-since", "origin/main"])
            .supports_lab_runner()
    );

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
fn test_lab_command_contracts_cover_hot_commands() {
    let supported = [
        (parsed_command(&["homeboy", "lint"]), "lint"),
        (parsed_command(&["homeboy", "test"]), "test"),
        (parsed_command(&["homeboy", "audit"]), "audit"),
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
        (parsed_command(&["homeboy", "trace"]), "trace"),
        (
            parsed_command(&["homeboy", "refactor", "--from", "audit"]),
            "refactor",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "dispatch", "--prompt", "cook"]),
            "agent-task dispatch/cook/loop/run-plan/retry --run",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "cook", "--prompt", "cook"]),
            "agent-task dispatch/cook/loop/run-plan/retry --run",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "loop",
                "--to-worktree",
                "homeboy@smoke",
                "--verify",
                "true",
                "--prompt",
                "cook",
            ]),
            "agent-task dispatch/cook/loop/run-plan/retry --run",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "run-plan", "--plan", "@plan.json"]),
            "agent-task dispatch/cook/loop/run-plan/retry --run",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "retry", "agent-task-123", "--run"]),
            "agent-task dispatch/cook/loop/run-plan/retry --run",
        ),
        (
            parsed_command(&["homeboy", "agent-task", "providers"]),
            "agent-task providers",
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
            "agent-task controller from-spec --resume/materialize",
        ),
        (
            parsed_command(&[
                "homeboy",
                "agent-task",
                "controller",
                "materialize",
                "loop.json",
            ]),
            "agent-task controller from-spec --resume/materialize",
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
    ];

    for (command, label) in supported {
        let contract = command.lab_contract().expect("hot contract");
        assert_eq!(contract.hot_label, label);
        assert!(
            lab_runner_summary_covers_contract_label(contract.hot_label),
            "Lab support summary omitted `{}`",
            contract.hot_label
        );
        assert_eq!(contract.portability, LabCommandPortability::Portable);
        assert_eq!(contract.source_path_mode, LabSourcePathMode::CwdOrPathFlag);
        assert_eq!(
            contract.workspace_mode_policy,
            LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot
        );
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

    // Changed-scope/local-only variants and non-gate commands are NOT
    // release gates.
    assert!(
        !parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"])
            .lab_contract()
            .expect("changed-scope lint contract")
            .routing_policy
            .release_gate
    );
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
        !parsed_command(&["homeboy", "agent-task", "dispatch", "--prompt", "cook"])
            .lab_contract()
            .expect("agent-task contract")
            .routing_policy
            .release_gate
    );

    for args in [
        ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "logs", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "artifacts", "agent-task-123"].as_slice(),
        ["homeboy", "agent-task", "review", "agent-task-123"].as_slice(),
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
        ["homeboy", "lint", "--changed-since", "origin/main"].as_slice(),
        ["homeboy", "lint", "--changed-only"].as_slice(),
        ["homeboy", "test", "--changed-since", "origin/main"].as_slice(),
    ] {
        let contract = parsed_command(args)
            .lab_contract()
            .expect("scoped hot command should have a Lab plan contract");
        assert!(matches!(
            contract.portability,
            LabCommandPortability::LocalOnly(_)
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
fn agent_task_git_checkout_policy_uses_default_backend_when_backend_is_omitted() {
    let command = parsed_command(&[
        "homeboy",
        "agent-task",
        "cook",
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
    let command = parsed_command(&["homeboy", "agent-task", "cook", "--prompt", "cook"]);
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
        "dispatch",
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
            .expect("changed-scope lint reason")
            .contains("Changed-scope lint runs stay local")
    );
    assert!(
        parsed_command(&["homeboy", "test", "--changed-since", "origin/main"])
            .lab_runner_unsupported_reason()
            .expect("changed-since test reason")
            .contains("test --changed-since")
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
