use clap::{CommandFactory, Parser};
use homeboy::cli_surface::{
    command_surface_from_with_depth, current_command_safety_manifest, current_command_surface, Cli,
    CommandSafetyEntry, CommandSafetyManifest, Commands,
};
use std::collections::BTreeSet;
use std::fs;
use std::sync::OnceLock;

#[test]
fn command_surface_tracks_representative_live_and_removed_paths() {
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
    assert!(!surface.contains_path(&["init"]));
    assert!(!surface.contains_path(&["version", "bump"]));
    assert!(!surface.contains_path(&["components"]));
    assert!(!surface.contains_path(&["stacks", "inspect"]));
    assert!(!surface.contains_path(&["audit", "code"]));
    assert!(!surface.contains_path(&["list"]));
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
fn agent_task_discovery_help_documents_typed_flags() {
    let mut root = Cli::command();
    let agent_task = root
        .find_subcommand_mut("agent-task")
        .expect("agent-task command");

    let list_help = agent_task
        .find_subcommand_mut("list")
        .expect("agent-task list command")
        .render_long_help()
        .to_string();
    assert!(list_help.contains("--limit <N>"));

    let active_help = agent_task
        .find_subcommand_mut("active")
        .expect("agent-task active command")
        .render_long_help()
        .to_string();
    assert!(active_help.contains("--limit <N>"));
    assert!(active_help.contains("--reconcile"));
    assert!(active_help.contains("--dry-run"));

    let latest_help = agent_task
        .find_subcommand_mut("latest")
        .expect("agent-task latest command")
        .render_long_help()
        .to_string();
    assert!(latest_help.contains("--limit <N>"));
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
fn command_index_matches_top_level_command_surface() {
    let manifest = command_safety_manifest();
    let documented = documented_command_index_entries();

    let extension_commands = BTreeSet::from(["cargo".to_string(), "wp".to_string()]);
    let expected: BTreeSet<String> = manifest
        .commands
        .iter()
        .filter(|entry| visible_manifest_entry_with_docs_path(entry))
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

    for command in &expected {
        assert!(
            command_doc_path(command).is_file(),
            "docs/commands/commands-index.md lists `{command}` but docs/commands/{command}.md is missing"
        );
    }
}

#[test]
fn command_safety_manifest_docs_paths_match_command_docs() {
    let manifest = command_safety_manifest();
    let hidden_top_level_commands = BTreeSet::from(["list"]);

    for entry in &manifest.commands {
        if entry.hidden {
            assert!(
                hidden_top_level_commands.contains(entry.name.as_str()),
                "hidden top-level command `{}` must be added to the explicit internal-command exemption list before it can skip command docs",
                entry.name
            );
            assert!(
                entry.docs.path.is_none(),
                "hidden internal command `{}` should not advertise a command docs path",
                entry.name
            );
            continue;
        }

        let Some(path) = &entry.docs.path else {
            continue;
        };
        assert_eq!(
            path,
            &format!("docs/commands/{}.md", entry.name),
            "visible top-level command `{}` has an unexpected docs path in the safety manifest",
            entry.name
        );
        assert!(
            command_doc_manifest_path(path).is_file(),
            "visible top-level command `{}` advertises `{path}` in the safety manifest, but that file is missing",
            entry.name
        );
    }
}

#[test]
fn visible_safety_manifest_entries_advertise_live_command_docs() {
    let manifest = command_safety_manifest();

    for entry in all_safety_entries(&manifest.commands) {
        if entry.hidden {
            continue;
        }

        let path =
            entry.docs.path.as_deref().unwrap_or_else(|| {
                panic!("visible command {:?} is missing docs metadata", entry.path)
            });
        let top_level = entry.path.first().expect("safety path should not be empty");
        assert_eq!(
            path,
            format!("docs/commands/{top_level}.md"),
            "visible command {:?} should point at its top-level command doc",
            entry.path
        );
        assert!(
            command_doc_manifest_path(path).is_file(),
            "visible command {:?} advertises `{path}` in the safety manifest, but that file is missing",
            entry.path
        );
    }
}

#[test]
fn mutating_safety_manifest_entries_advertise_apply_or_are_allowlisted() {
    let manifest = command_safety_manifest();
    let explicit_no_apply_surface = BTreeSet::from([
        "component create".to_string(),
        "component delete".to_string(),
        "component rename".to_string(),
        "component set".to_string(),
        "component setup".to_string(),
        "config remove".to_string(),
        "config reset".to_string(),
        "config set".to_string(),
        "extension install".to_string(),
        "extension install-for-component".to_string(),
        "extension refresh".to_string(),
        "extension relink".to_string(),
        "extension set".to_string(),
        "extension setup".to_string(),
        "extension uninstall".to_string(),
        "extension update".to_string(),
        "file mkdir".to_string(),
        "file rename".to_string(),
        "project components attach-path".to_string(),
        "project components clear".to_string(),
        "project components remove".to_string(),
        "project components set".to_string(),
        "project create".to_string(),
        "project delete".to_string(),
        "project init".to_string(),
        "project pin add".to_string(),
        "project pin remove".to_string(),
        "project pin update".to_string(),
        "project remove".to_string(),
        "project rename".to_string(),
        "project set".to_string(),
        "runs import".to_string(),
        "server connect".to_string(),
        "server create".to_string(),
        "server delete".to_string(),
        "server disconnect".to_string(),
        "server key generate".to_string(),
        "server key import".to_string(),
        "server key unset".to_string(),
        "server key use".to_string(),
        "server set".to_string(),
        "triage".to_string(),
    ]);

    let missing_safety_surface: Vec<_> = all_safety_entries(&manifest.commands)
        .into_iter()
        .filter(|entry| entry.mutates)
        .filter(|entry| {
            !entry.dry_run.supported
                && !entry.output.notes.contains("--apply")
                && !entry.output.notes.contains("--dry-run")
                && !entry.output.notes.contains("--check")
                && !entry.dangerous_flags.iter().any(|flag| flag == "--apply")
                && !explicit_no_apply_surface.contains(&entry.path.join(" "))
        })
        .map(|entry| entry.path.join(" "))
        .collect();

    assert!(
        missing_safety_surface.is_empty(),
        "mutating safety-manifest entries must advertise dry-run/apply/check metadata or be explicitly allowlisted: {missing_safety_surface:?}"
    );
}

#[test]
fn command_docs_files_match_command_index_snapshot() {
    let documented = documented_command_index_entries();
    let companion_topics = BTreeSet::from([
        "audit-rules".to_string(),
        "commands-index".to_string(),
        "rig-spec".to_string(),
    ]);
    let hidden_documented_commands: BTreeSet<String> = BTreeSet::new();
    let docs_files = documented_command_doc_files();

    // Guard against a vacuously-passing snapshot: the index and the docs
    // directory must both be populated, otherwise the set algebra below would
    // pass trivially against empty inputs.
    assert!(
        documented.len() >= 10,
        "commands-index.md should enumerate the real command surface, found only {} entries: {documented:?}",
        documented.len()
    );
    assert!(
        docs_files.len() > documented.len(),
        "docs/commands/ should contain one file per indexed command plus companion topics, found {} docs vs {} indexed",
        docs_files.len(),
        documented.len()
    );

    // Tie the snapshot to the live product command surface: every visible
    // top-level command exposed by the safety manifest must be both indexed and
    // backed by a docs file. This anchors the assertion to real product
    // behavior rather than to fixture-only set arithmetic.
    let manifest = command_safety_manifest();
    let live_top_level: BTreeSet<String> = manifest
        .commands
        .iter()
        .filter(|entry| visible_manifest_entry_with_docs_path(entry))
        .map(|entry| entry.name.clone())
        .collect();
    assert!(
        live_top_level.contains("audit") && live_top_level.contains("report"),
        "expected representative live commands `audit` and `report` in the safety manifest: {live_top_level:?}"
    );

    let live_missing_from_index: Vec<_> = live_top_level.difference(&documented).cloned().collect();
    assert!(
        live_missing_from_index.is_empty(),
        "commands-index.md is missing live top-level commands from the safety manifest: {live_missing_from_index:?}"
    );

    let live_missing_docs: Vec<_> = live_top_level
        .iter()
        .filter(|command| !docs_files.contains(*command))
        .cloned()
        .collect();
    assert!(
        live_missing_docs.is_empty(),
        "live top-level commands are missing docs/commands/<command>.md files: {live_missing_docs:?}"
    );

    // Every command doc file must either be an indexed command or a known
    // companion topic — no orphaned docs allowed.
    let missing_from_index: Vec<_> = docs_files
        .difference(&documented)
        .filter(|entry| {
            !companion_topics.contains(*entry) && !hidden_documented_commands.contains(*entry)
        })
        .cloned()
        .collect();
    assert!(
        missing_from_index.is_empty(),
        "docs/commands/*.md contains command docs that are not listed in commands-index.md: {missing_from_index:?}"
    );

    // Companion topics are documentation-only — they must ship a doc file and
    // must never leak into the command index.
    for topic in &companion_topics {
        assert!(
            docs_files.contains(topic),
            "expected companion topic `{topic}` to have a docs/commands/{topic}.md file"
        );
    }
    assert!(
        documented.contains("audit") && documented.contains("report"),
        "commands-index.md should document representative commands `audit` and `report`: {documented:?}"
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
        "/home/user/Developer",
        "python3",
        "-c",
        "print('hello')",
    ])
    .expect("runner exec --raw should parse before the trailing remote command");
}

#[test]
fn runner_env_rejects_legacy_show_values_flag() {
    assert!(
        Cli::try_parse_from(["homeboy", "runner", "env", "homeboy-lab", "--show-values"]).is_err(),
        "runner env must not expose raw environment values"
    );
}

#[test]
fn docs_cover_focused_command_surface_cleanup_targets() {
    let auth = include_str!("../docs/commands/auth.md");
    assert!(auth.contains("`profile`"));
    assert!(auth.contains("set-basic"));
    assert!(auth.contains("set-bearer"));

    let self_docs = include_str!("../docs/commands/self.md");
    assert!(self_docs.contains("`identity`"));
    assert!(self_docs.contains("`doctor`"));
    assert!(self_docs.contains("`cleanup-runtime-tmp`"));
    assert!(self_docs.contains("--older-than-days"));
    assert!(self_docs.contains("--apply"));

    let extension = include_str!("../docs/commands/extension.md");
    assert!(extension.contains("install-for-component --source <source> [--path <component_path>]"));
    assert!(!extension.contains("install-for-component") || !extension.contains("--revision"));

    let report = include_str!("../docs/commands/report.md");
    assert!(report.contains("`performance-digest`"));

    let runs = include_str!("../docs/commands/runs.md");
    assert!(runs.contains("## Mutating Subcommands"));
    assert!(runs.contains("artifact cleanup-persisted"));
    assert!(runs.contains("`reconcile`"));
    assert!(runs.contains("`latest-run`"));
    assert!(runs.contains("`query`"));
    assert!(runs.contains("`drift`"));
    assert!(runs.contains("`loop-sync`"));

    let cargo = include_str!("../docs/commands/cargo.md");
    assert!(cargo.contains("extension-provided"));
    let wp = include_str!("../docs/commands/wp.md");
    assert!(wp.contains("extension-provided"));

    for args in [
        ["homeboy", "auth", "profile", "set-basic", "dev"].as_slice(),
        ["homeboy", "auth", "profile", "set-bearer", "dev"].as_slice(),
        ["homeboy", "self", "identity"].as_slice(),
        ["homeboy", "self", "doctor"].as_slice(),
        [
            "homeboy",
            "self",
            "cleanup-runtime-tmp",
            "--older-than-days",
            "14",
        ]
        .as_slice(),
        [
            "homeboy",
            "report",
            "performance-digest",
            "--output-dir",
            ".",
        ]
        .as_slice(),
        ["homeboy", "runs", "artifact", "get", "run-1", "artifact-1"].as_slice(),
        ["homeboy", "runs", "latest-run"].as_slice(),
        ["homeboy", "runs", "evidence", "run-1"].as_slice(),
        ["homeboy", "runs", "findings", "run-1"].as_slice(),
        ["homeboy", "runs", "query", "--select", "$.status"].as_slice(),
        ["homeboy", "runs", "drift", "--metric", "$.status"].as_slice(),
        ["homeboy", "runs", "loop-sync", ".", "--dry-run"].as_slice(),
        [
            "homeboy",
            "runs",
            "artifact",
            "cleanup-persisted",
            "--older-than-days",
            "30",
        ]
        .as_slice(),
    ] {
        Cli::try_parse_from(args).unwrap_or_else(|error| {
            panic!("documented cleanup target failed to parse: {args:?}\n{error}")
        });
    }

    assert!(
        Cli::try_parse_from([
            "homeboy",
            "extension",
            "install-for-component",
            "--source",
            "https://example.com/extensions.git",
            "--revision",
            "main",
        ])
        .is_err(),
        "extension install-for-component should not advertise or accept stale --revision/--ref flags"
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

fn command_safety_manifest() -> &'static CommandSafetyManifest {
    static MANIFEST: OnceLock<CommandSafetyManifest> = OnceLock::new();
    MANIFEST.get_or_init(current_command_safety_manifest)
}

fn documented_command_doc_files() -> BTreeSet<String> {
    let commands_dir = command_doc_path("");
    fs::read_dir(&commands_dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", commands_dir.display()))
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .collect()
}

fn visible_manifest_entry_with_docs_path(entry: &CommandSafetyEntry) -> bool {
    !entry.hidden && entry.docs.path.is_some()
}

fn all_safety_entries(entries: &[CommandSafetyEntry]) -> Vec<&CommandSafetyEntry> {
    let mut flattened = Vec::new();
    for entry in entries {
        flattened.push(entry);
        flattened.extend(all_safety_entries(&entry.subcommands));
    }
    flattened
}

fn command_doc_manifest_path(path: &str) -> std::path::PathBuf {
    let mut root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.push(path);
    root
}

fn command_doc_path(command: &str) -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("docs/commands");
    if !command.is_empty() {
        path.push(format!("{command}.md"));
    }
    path
}
