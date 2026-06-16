use clap::{CommandFactory, Parser};
use homeboy::cli_surface::{current_command_surface, Cli};
use std::collections::BTreeSet;

#[test]
fn includes_current_top_level_commands() {
    let surface = current_command_surface();

    assert!(surface.contains_path(&["audit"]));
    assert!(surface.contains_path(&["daemon"]));
    assert!(surface.contains_path(&["deps"]));
    assert!(surface.contains_path(&["git"]));
    assert!(surface.contains_path(&["http"]));
    assert!(surface.contains_path(&["self"]));
    assert!(surface.contains_path(&["stack"]));
    assert!(surface.contains_path(&["report"]));
    assert!(surface.contains_path(&["upgrade"]));
    assert!(!surface.contains_path(&["init"]));
    assert!(!surface.contains_path(&["update"]));
    assert!(!surface.contains_path(&["transfer"]));
}

#[test]
fn includes_first_level_subcommands() {
    let surface = current_command_surface();

    assert!(surface.contains_path(&["git", "status"]));
    assert!(surface.contains_path(&["http", "get"]));
    assert!(surface.contains_path(&["deps", "status"]));
    assert!(surface.contains_path(&["deps", "update"]));
    assert!(surface.contains_path(&["daemon", "serve"]));
    assert!(surface.contains_path(&["self", "status"]));
    assert!(surface.contains_path(&["stack", "inspect"]));
    assert!(surface.contains_path(&["report", "failure-digest"]));
    assert!(surface.contains_path(&["report", "performance-digest"]));
    assert!(surface.contains_path(&["report", "bench-coverage"]));
    assert!(surface.contains_path(&["file", "download"]));
    assert!(surface.contains_path(&["file", "mkdir"]));
    assert!(surface.contains_path(&["file", "upload"]));
    assert!(surface.contains_path(&["file", "copy"]));
    assert!(surface.contains_path(&["file", "sync"]));
    assert!(surface.contains_path(&["runner", "job"]));
    assert!(surface.contains_path(&["agent-task", "loop"]));
    assert!(surface.contains_path(&["version", "show"]));
    assert!(surface.contains_path(&["worktree", "create"]));
    assert!(surface.contains_path(&["worktree", "remove"]));
    assert!(surface.contains_path(&["tunnel", "preview-consumer"]));
    assert!(!surface.contains_path(&["version", "bump"]));
}

#[test]
fn excludes_removed_cli_aliases() {
    let surface = current_command_surface();

    assert!(!surface.contains_path(&["components"]));
    assert!(!surface.contains_path(&["dependencies"]));
    assert!(!surface.contains_path(&["rigs"]));
    assert!(!surface.contains_path(&["stacks", "inspect"]));
}

#[test]
fn rejects_stale_or_deeper_paths() {
    let surface = current_command_surface();

    assert!(!surface.contains_path(&["supports"]));
    assert!(!surface.contains_path(&["audit", "code"]));
    assert!(!surface.contains_path(&["audit", "docs"]));
    assert!(!surface.contains_path(&["audit", "structure"]));
    assert!(!surface.contains_path(&["stack", "inspect", "extra"]));
}

#[test]
fn command_index_matches_top_level_command_surface() {
    let surface = current_command_surface();
    let documented = documented_command_index_entries();

    let extension_commands = BTreeSet::from(["cargo".to_string(), "wp".to_string()]);
    let expected: BTreeSet<String> = surface
        .commands
        .iter()
        .map(|entry| entry.name.clone())
        .chain(extension_commands.iter().cloned())
        .collect();

    let missing: Vec<_> = expected.difference(&documented).cloned().collect();
    let stale: Vec<_> = documented.difference(&expected).cloned().collect();

    assert!(
        missing.is_empty(),
        "docs/commands/commands-index.md is missing top-level commands: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "docs/commands/commands-index.md lists stale top-level commands: {stale:?}"
    );
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
fn runner_exec_raw_command_parses_before_trailing_command() {
    Cli::try_parse_from([
        "homeboy",
        "runner",
        "exec",
        "homeboy-lab",
        "--raw",
        "--cwd",
        "/home/chubes/Developer",
        "python3",
        "-c",
        "print('hello')",
    ])
    .expect("runner exec --raw should parse before the trailing remote command");
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

fn documented_command_index_entries() -> BTreeSet<String> {
    let index = include_str!("../docs/commands/commands-index.md");
    let command_section = index.split("Related:").next().unwrap_or(index);

    command_section
        .lines()
        .filter_map(|line| line.strip_prefix("- ["))
        .filter_map(|rest| rest.split(']').next())
        .map(str::to_string)
        .collect()
}
