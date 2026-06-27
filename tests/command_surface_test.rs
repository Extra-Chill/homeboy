use clap::{CommandFactory, Parser};
use homeboy::cli_surface::{
    command_surface_doctor_report, command_surface_from_with_depth,
    current_command_safety_manifest, current_command_surface, Cli, CommandSafetyManifest, Commands,
};
use std::collections::BTreeSet;
use std::sync::OnceLock;

#[test]
fn command_surface_tracks_representative_live_paths() {
    let surface = current_command_surface();

    assert!(surface.contains_path(&["audit"]));
    assert!(surface.contains_path(&["manifest"]));
    assert!(surface.contains_path(&["report"]));
    assert!(surface.contains_path(&["git", "status"]));
    assert!(surface.contains_path(&["http", "get"]));
    assert!(surface.contains_path(&["report", "failure-digest"]));
    assert!(surface.contains_path(&["runner", "job"]));
    assert!(surface.contains_path(&["agent-task", "controller", "run-next"]));
    assert!(surface.contains_path(&["agent-task", "auth", "status"]));
    assert!(surface.contains_path(&["agent-task", "prompts", "save"]));
    assert!(surface.contains_path(&["runner", "job", "logs"]));
    assert!(!surface.contains_path(&["runner", "job", "logs", "extra"]));
}

#[test]
fn agent_task_prompt_store_commands_parse() {
    Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "prompts",
        "save",
        "issue-123",
        "--input",
        "@prompt.md",
    ])
    .expect("agent-task prompts save should parse");
    Cli::try_parse_from(["homeboy", "agent-task", "prompts", "list"])
        .expect("agent-task prompts list should parse");
    Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--repo",
        "homeboy",
        "--to-worktree",
        "homeboy@prompt-ref-test",
        "--prompt",
        "prompt:issue-123",
        "--verify",
        "homeboy test homeboy",
    ])
    .expect("stored prompt refs should parse as cook prompt values");
}

#[test]
fn agent_task_dispatch_is_not_public_cli_surface() {
    let surface = current_command_surface();

    assert!(!surface.contains_path(&["agent-task", "dispatch"]));
    assert!(Cli::try_parse_from(["homeboy", "agent-task", "dispatch", "--prompt", "x"]).is_err());
}

#[test]
fn agent_task_discovery_commands_use_typed_args() {
    let list = Cli::try_parse_from(["homeboy", "agent-task", "list", "--limit", "5"])
        .expect("agent-task list --limit should parse");
    let active = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "active",
        "--limit=7",
        "--reconcile",
        "--dry-run",
    ])
    .expect("agent-task active typed flags should parse");
    let latest = Cli::try_parse_from(["homeboy", "agent-task", "latest", "--limit", "1"])
        .expect("agent-task latest --limit should parse");

    match list.command {
        Commands::AgentTask(args) => match args.command {
            homeboy::commands::agent_task::AgentTaskCommand::List(args) => {
                assert_eq!(args.limit, Some(5));
            }
            other => panic!("expected agent-task list, got {other:?}"),
        },
        _ => panic!("expected agent-task command"),
    }

    match active.command {
        Commands::AgentTask(args) => match args.command {
            homeboy::commands::agent_task::AgentTaskCommand::Active(args) => {
                assert_eq!(args.limit, Some(7));
                assert!(args.reconcile);
                assert!(args.dry_run);
            }
            other => panic!("expected agent-task active, got {other:?}"),
        },
        _ => panic!("expected agent-task command"),
    }

    match latest.command {
        Commands::AgentTask(args) => match args.command {
            homeboy::commands::agent_task::AgentTaskCommand::Latest(args) => {
                assert_eq!(args.limit, Some(1));
            }
            other => panic!("expected agent-task latest, got {other:?}"),
        },
        _ => panic!("expected agent-task command"),
    }

    assert!(Cli::try_parse_from(["homeboy", "agent-task", "active", "--dry-run"]).is_err());
}

#[test]
fn agent_task_tool_bridge_stays_hidden_but_parseable() {
    let surface = current_command_surface();

    assert!(!surface.contains_path(&["agent-task", "tool"]));
    Cli::try_parse_from(["homeboy", "agent-task", "tool", "dispatch"])
        .expect("hidden agent-task tool bridge should stay parseable for runtimes");

    let docs = include_str!("../docs/commands/agent-task.md");
    assert!(docs.contains("## Internal Bridge"));
    assert!(docs.contains("hidden provider-runtime bridge"));
}

#[test]
fn agent_task_controller_events_command_parses() {
    Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "controller",
        "events",
        "loop-1",
        "--event-type",
        "task.completed",
        "--event-id",
        "event-1",
        "--event-key",
        "task#1",
        "--entity-id",
        "entity-1",
        "--payload",
        r#"{"status":"ok"}"#,
    ])
    .expect("agent-task controller events should parse as the generic event primitive");
}

#[test]
fn agent_task_controller_resume_dispatch_defaults_parse() {
    Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "controller",
        "resume",
        "loop-1",
        "--dispatch-backend",
        "sandbox-provider",
        "--dispatch-selector",
        "homeboy-lab",
        "--dispatch-model",
        "test-model",
    ])
    .expect("agent-task controller resume should accept generic dispatch defaults");
}

#[test]
fn command_surface_depth_is_configurable() {
    let surface = command_surface_from_with_depth(Cli::command(), 1);

    assert!(surface.contains_path(&["runner", "job"]));
    assert!(!surface.contains_path(&["runner", "job", "logs"]));
}

#[test]
fn manifest_command_exposes_recursive_safety_manifest() {
    let cli = Cli::try_parse_from(["homeboy", "manifest"]).expect("manifest command should parse");
    assert!(matches!(cli.command, Commands::Manifest(_)));
    assert!(Cli::try_parse_from(["homeboy", "list", "--json"]).is_err());

    let value =
        serde_json::to_value(command_safety_manifest()).expect("safety manifest should serialize");
    assert_eq!(value["commands"][0]["path"].as_array().unwrap().len(), 1);
    assert!(value["commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["subcommands"]
            .as_array()
            .is_some_and(|subcommands| !subcommands.is_empty())));
    assert!(value["commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry.get("mutates").is_some()
            && entry.get("operator").is_some()
            && entry.get("dangerous_flags").is_some()));
}

#[test]
fn command_surface_doctor_report_agrees_for_matching_sets() {
    let source = BTreeSet::from(["fuzz".to_string(), "manifest".to_string()]);
    let docs = BTreeSet::from([
        "cargo".to_string(),
        "fuzz".to_string(),
        "manifest".to_string(),
        "wp".to_string(),
    ]);
    let help = BTreeSet::from(["fuzz".to_string(), "manifest".to_string()]);
    let extension_docs = BTreeSet::from(["cargo".to_string(), "wp".to_string()]);

    let report = command_surface_doctor_report(source, docs, help, extension_docs);

    assert!(report.agrees);
    assert!(report.drift_notes.is_empty());
    assert_eq!(report.runtime_extension_docs, vec!["cargo", "wp"]);
}

#[test]
fn command_surface_doctor_report_detects_docs_and_help_mismatches() {
    let source = BTreeSet::from(["fuzz".to_string(), "manifest".to_string()]);
    let docs = BTreeSet::from(["lab".to_string(), "manifest".to_string(), "wp".to_string()]);
    let help = BTreeSet::from(["fuzz".to_string(), "lab".to_string()]);
    let extension_docs = BTreeSet::from(["wp".to_string()]);

    let report = command_surface_doctor_report(source, docs, help, extension_docs);

    assert!(!report.agrees);
    assert_eq!(report.missing_from_docs_index, vec!["fuzz"]);
    assert_eq!(report.stale_docs_index, vec!["lab"]);
    assert_eq!(report.missing_from_help, vec!["manifest"]);
    assert_eq!(report.missing_from_source_registry, vec!["lab"]);
    assert!(report
        .drift_notes
        .iter()
        .any(|note| note.contains("missing from docs") && note.contains("fuzz")));
    assert!(report
        .drift_notes
        .iter()
        .any(|note| note.contains("stale commands") && note.contains("lab")));
}

#[test]
fn upgrade_runner_selector_does_not_collide_with_global_lab_runner_flag() {
    let root = Cli::command();
    let root_flags: BTreeSet<String> = root
        .get_arguments()
        .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
        .collect();
    let upgrade = root
        .find_subcommand("upgrade")
        .expect("upgrade command")
        .clone();
    let upgrade_flags: BTreeSet<String> = upgrade
        .get_arguments()
        .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
        .collect();

    assert!(root_flags.contains("--runner"));
    assert!(upgrade_flags.contains("--upgrade-runner"));
    Cli::try_parse_from(["homeboy", "upgrade", "--upgrade-runner", "homeboy-lab"])
        .expect("upgrade runner selector should parse without colliding with global --runner");
}

#[test]
fn runner_job_logs_command_parses() {
    Cli::try_parse_from([
        "homeboy",
        "runner",
        "job",
        "logs",
        "homeboy-lab",
        "00000000-0000-0000-0000-000000000000",
        "--follow",
    ])
    .expect("runner job logs command should parse");
}

#[test]
fn runner_job_broker_wrapper_commands_parse() {
    Cli::try_parse_from(["homeboy", "runner", "job", "reconcile", "homeboy-lab"])
        .expect("runner job reconcile command should parse");
    Cli::try_parse_from([
        "homeboy",
        "runner",
        "job",
        "artifacts",
        "homeboy-lab",
        "00000000-0000-0000-0000-000000000000",
        "report.txt",
    ])
    .expect("runner job artifacts command should parse");
}

#[test]
fn runner_exec_flags_parse_before_trailing_command() {
    // `runner exec` collects the remote command via `trailing_var_arg`, so every
    // exec-side flag must bind before the trailing argv begins. Exercise the
    // distinct flags that share this ordering invariant in one place.
    for args in [
        [
            "homeboy", "runner", "exec", "homeboy-lab", "--raw", "--cwd", "/runner/workspaces",
            "python3", "-c", "print('hello')",
        ]
        .as_slice(),
        [
            "homeboy", "runner", "exec", "homeboy-lab", "--run-id", "ssi-fixture-matrix-summary",
            "--cwd", "/runner/workspaces", "homeboy", "trace", "matrix", "summary",
        ]
        .as_slice(),
        [
            "homeboy", "runner", "exec", "homeboy-lab", "--run-id", "runner-exec-artifact-fixture",
            "--artifact", "output/report.json", "--cwd", "/runner/workspaces", "homeboy", "trace",
            "matrix", "summary",
        ]
        .as_slice(),
        [
            "homeboy", "runner", "exec", "homeboy-lab", "--run-id",
            "runner-exec-artifact-dir-fixture", "--artifact-dir", "output", "--cwd",
            "/runner/workspaces", "homeboy", "trace", "matrix", "summary",
        ]
        .as_slice(),
    ] {
        Cli::try_parse_from(args).unwrap_or_else(|error| {
            panic!("runner exec flag should bind before the trailing command: {args:?}\n{error}")
        });
    }
}

#[test]
fn runner_env_rejects_legacy_show_values_flag() {
    assert!(
        Cli::try_parse_from(["homeboy", "runner", "env", "homeboy-lab", "--show-values"]).is_err(),
        "runner env must not expose raw environment values"
    );
}

#[test]
fn agent_task_auth_status_accepts_global_runner_and_secret_env() {
    Cli::try_parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "agent-task",
        "auth",
        "status",
        "--secret-env",
        "OPENAI_API_KEY",
    ])
    .expect("agent-task auth status should accept global --runner with auth --secret-env");
}

#[test]
fn rig_install_documents_reinstall_semantics() {
    let mut root = Cli::command();
    let help = root
        .find_subcommand_mut("rig")
        .expect("rig command")
        .find_subcommand_mut("install")
        .expect("rig install command")
        .render_long_help()
        .to_string();

    assert!(help.contains("--reinstall"));
    assert!(help.contains("refresh an existing matching rig install"));
    assert!(help.contains("Refuses user-owned conflicts"));
    assert!(!help.contains("--force-hot"));
}

#[test]
fn rig_install_accepts_reinstall_and_force_alias() {
    Cli::try_parse_from([
        "homeboy",
        "rig",
        "install",
        "./packages/studio",
        "--reinstall",
    ])
    .expect("rig install --reinstall should parse");
    Cli::try_parse_from(["homeboy", "rig", "install", "./packages/studio", "--force"])
        .expect("rig install --force should parse as reinstall intent");
}

#[test]
fn rig_install_unknown_force_like_flag_does_not_suggest_force_hot() {
    let error =
        match Cli::try_parse_from(["homeboy", "rig", "install", "./packages/studio", "--forc"]) {
            Ok(_) => panic!("mistyped force flag should error"),
            Err(error) => error,
        };
    let message = error.to_string();

    assert!(message.contains("--force") || message.contains("--reinstall"));
    assert!(!message.contains("--force-hot"));
}

fn command_safety_manifest() -> &'static CommandSafetyManifest {
    static MANIFEST: OnceLock<CommandSafetyManifest> = OnceLock::new();
    MANIFEST.get_or_init(current_command_safety_manifest)
}
