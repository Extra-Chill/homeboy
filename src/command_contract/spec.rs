//! Shared top-level command metadata.
//!
//! This is the first narrow `CommandSpec` slice: top-level metadata that is
//! consumed by output routing, safety/docs manifest derivation, and command
//! lookup without changing parsed CLI behavior.

use super::output::{CommandDispatchFamily, CommandJsonFamily};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub json_family: CommandJsonFamily,
    pub docs_slug: Option<&'static str>,
    pub output_notes: &'static str,
    pub lab_supported: bool,
    pub lab_notes: &'static str,
}

pub type CommandRegistryEntry = CommandSpec;

pub const DEFAULT_LAB_UNSUPPORTED_NOTES: &str =
    "not declared as Lab-routable in the command registry";

impl CommandSpec {
    pub fn docs_path(&self) -> Option<String> {
        self.docs_slug
            .map(|slug| format!("docs/commands/{slug}.md"))
    }
}

const fn command_spec(name: &'static str, json_family: CommandJsonFamily) -> CommandSpec {
    CommandSpec {
        name,
        json_family,
        docs_slug: Some(name),
        output_notes: "standard CLI output contract",
        lab_supported: false,
        lab_notes: DEFAULT_LAB_UNSUPPORTED_NOTES,
    }
}

const fn command_spec_with_output_notes(
    name: &'static str,
    json_family: CommandJsonFamily,
    output_notes: &'static str,
) -> CommandSpec {
    CommandSpec {
        output_notes,
        ..command_spec(name, json_family)
    }
}

const fn lab_command_spec_with_output_notes(
    name: &'static str,
    json_family: CommandJsonFamily,
    lab_notes: &'static str,
    output_notes: &'static str,
) -> CommandSpec {
    CommandSpec {
        output_notes,
        ..lab_command_spec(name, json_family, lab_notes)
    }
}

const fn lab_command_spec(
    name: &'static str,
    json_family: CommandJsonFamily,
    lab_notes: &'static str,
) -> CommandSpec {
    CommandSpec {
        lab_supported: true,
        lab_notes,
        ..command_spec(name, json_family)
    }
}

const fn manifest_command_spec() -> CommandSpec {
    CommandSpec {
        output_notes:
            "recursive command safety, docs, output, and Lab metadata in the standard JSON envelope",
        ..command_spec("manifest", CommandJsonFamily::Workspace)
    }
}

pub const COMMAND_SPECS: &[CommandSpec] = &[
    lab_command_spec(
        "agent-task",
        CommandJsonFamily::Workspace,
        "Lab runner routing covers portable, explicit-runner, and runner-resident agent-task workflows",
    ),
    command_spec("project", CommandJsonFamily::Workspace),
    command_spec("ssh", CommandJsonFamily::Ops),
    command_spec("server", CommandJsonFamily::Ops),
    lab_command_spec(
        "test",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for test runs",
    ),
    lab_command_spec(
        "bench",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for benchmark runs",
    ),
    lab_command_spec(
        "fuzz",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for fuzz runs",
    ),
    lab_command_spec_with_output_notes(
        "trace",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for trace runs",
        "runs trace workflows and records observation artifacts unless using read-only subcommands",
    ),
    command_spec("observe", CommandJsonFamily::Quality),
    lab_command_spec_with_output_notes(
        "lint",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for changed-scope lint runs",
        "runs lint workflows; pass --fix to apply auto-fixable findings in place",
    ),
    command_spec("db", CommandJsonFamily::Ops),
    command_spec("deps", CommandJsonFamily::Ops),
    command_spec("ci", CommandJsonFamily::Ops),
    command_spec("doctor", CommandJsonFamily::Ops),
    command_spec("file", CommandJsonFamily::Ops),
    command_spec("fleet", CommandJsonFamily::Ops),
    command_spec("logs", CommandJsonFamily::Ops),
    command_spec("triage", CommandJsonFamily::Ops),
    command_spec("deploy", CommandJsonFamily::Ops),
    command_spec("component", CommandJsonFamily::Workspace),
    command_spec("config", CommandJsonFamily::Workspace),
    command_spec("daemon", CommandJsonFamily::Ops),
    command_spec("extension", CommandJsonFamily::Workspace),
    command_spec("status", CommandJsonFamily::Ops),
    command_spec("docs", CommandJsonFamily::Workspace),
    manifest_command_spec(),
    command_spec("changelog", CommandJsonFamily::Workspace),
    command_spec_with_output_notes(
        "cleanup",
        CommandJsonFamily::Workspace,
        "cleanup subcommands report plans by default and require --apply for removals",
    ),
    command_spec("git", CommandJsonFamily::Ops),
    command_spec("issues", CommandJsonFamily::Ops),
    command_spec("version", CommandJsonFamily::Workspace),
    command_spec("build", CommandJsonFamily::Workspace),
    command_spec("changes", CommandJsonFamily::Workspace),
    command_spec_with_output_notes(
        "release",
        CommandJsonFamily::Workspace,
        "release execution mutates git tags/releases and may deploy; use --dry-run to plan and --apply for risky modes",
    ),
    command_spec("report", CommandJsonFamily::Workspace),
    lab_command_spec(
        "review",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for release-gate review runs",
    ),
    lab_command_spec(
        "audit",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for audit source runs",
    ),
    command_spec("audit-baseline", CommandJsonFamily::Quality),
    lab_command_spec_with_output_notes(
        "refactor",
        CommandJsonFamily::Workspace,
        "portable Lab offload is available for refactor source runs",
        "refactor subcommands can rewrite source files; use planning/dry-run modes where available",
    ),
    command_spec("refs", CommandJsonFamily::Workspace),
    lab_command_spec(
        "rig",
        CommandJsonFamily::Workspace,
        "portable Lab offload is available for rig check workflows",
    ),
    command_spec("runner", CommandJsonFamily::Workspace),
    command_spec("runtime", CommandJsonFamily::Workspace),
    command_spec("worktree", CommandJsonFamily::Workspace),
    lab_command_spec(
        "tunnel",
        CommandJsonFamily::Workspace,
        "Lab runner routing covers tunnel preview and service workflows",
    ),
    command_spec("runs", CommandJsonFamily::Workspace),
    command_spec("self", CommandJsonFamily::Ops),
    command_spec("stack", CommandJsonFamily::Workspace),
    command_spec_with_output_notes(
        "undo",
        CommandJsonFamily::Workspace,
        "restores files from the latest or selected undo snapshot",
    ),
    command_spec("auth", CommandJsonFamily::Ops),
    command_spec("api", CommandJsonFamily::Ops),
    command_spec("http", CommandJsonFamily::Ops),
    command_spec_with_output_notes(
        "upgrade",
        CommandJsonFamily::Ops,
        "upgrades the active Homeboy binary, extensions, runners, and services unless --check or skip flags are used",
    ),
];

pub const COMMAND_REGISTRY: &[CommandRegistryEntry] = COMMAND_SPECS;

pub fn registered_command(name: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS.iter().find(|entry| entry.name == name)
}

pub fn registered_command_json_family(name: &str) -> Option<CommandJsonFamily> {
    registered_command(name).map(|entry| entry.json_family)
}

pub fn registered_command_dispatch_family(name: &str) -> Option<CommandDispatchFamily> {
    registered_command_json_family(name).map(Into::into)
}
