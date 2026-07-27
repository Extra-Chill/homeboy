//! Command-safety-manifest derivation.
//!
//! This module owns the logic that walks the clap-derived
//! [`CommandSurface`](crate::cli_surface::CommandSurface) and produces a
//! recursive [`CommandSafetyManifest`](crate::cli_surface::CommandSafetyManifest):
//! per-command mutation/operator classification, dry-run flags, structured
//! output notes, Lab metadata, docs paths, and dynamic (extension) command
//! overlays.
//!
//! The clap argument shapes themselves stay in [`crate::cli_surface`]; only the
//! *derivation* of safety metadata from those shapes lives here. The public
//! entry points are re-exported from `cli_surface` so call sites are unchanged.

use crate::cli_surface::{
    current_command_surface, CommandDocsMetadata, CommandDryRunMetadata, CommandLabMetadata,
    CommandOutputMetadata, CommandSafetyAuditFinding, CommandSafetyAuditReport, CommandSafetyEntry,
    CommandSafetyManifest, CommandSurface, CommandSurfaceEntry, DynamicCommandDescriptor,
};
use crate::command_contract::registered_command;

pub fn current_command_safety_manifest() -> CommandSafetyManifest {
    command_safety_manifest_from(current_command_surface())
}

pub fn command_safety_manifest_from(surface: CommandSurface) -> CommandSafetyManifest {
    command_safety_manifest_from_dynamic(surface, &[])
}

pub fn command_safety_manifest_from_dynamic(
    surface: CommandSurface,
    dynamic_commands: &[DynamicCommandDescriptor],
) -> CommandSafetyManifest {
    CommandSafetyManifest {
        commands: surface
            .commands
            .iter()
            .map(|entry| command_safety_entry(entry, &[], dynamic_commands))
            .collect(),
    }
}

pub fn command_safety_manifest_audit(manifest: &CommandSafetyManifest) -> CommandSafetyAuditReport {
    let mut missing_action_metadata = Vec::new();

    for entry in flatten_manifest_entries(&manifest.commands) {
        if !entry.hidden && entry.mutates && !entry_has_action_metadata(entry) {
            missing_action_metadata.push(CommandSafetyAuditFinding {
                path: entry.path.clone(),
                reason: "visible mutating command lacks dry-run, dangerous/apply flag, or risk exemption metadata".to_string(),
            });
        }
    }

    CommandSafetyAuditReport {
        report_only: true,
        missing_action_metadata,
    }
}

fn command_safety_entry(
    entry: &CommandSurfaceEntry,
    parent_path: &[String],
    dynamic_commands: &[DynamicCommandDescriptor],
) -> CommandSafetyEntry {
    let mut path = parent_path.to_vec();
    path.push(entry.name.clone());
    let mut safety = command_safety_metadata(&path);
    let dynamic_command = dynamic_command_for_path(&path, dynamic_commands);

    if let Some(dynamic_safety) = dynamic_command.and_then(|command| command.safety.as_ref()) {
        safety.mutates = dynamic_safety.mutates;
        safety.operator = dynamic_safety.operator;
        safety.output_notes = dynamic_safety.output_notes;
        safety.lab_notes = dynamic_safety.lab_notes;
        safety.dangerous_flags = dynamic_safety.dangerous_flags.clone();
    }

    CommandSafetyEntry {
        name: entry.name.clone(),
        aliases: entry.visible_aliases.clone(),
        hidden: entry.hidden,
        path: path.clone(),
        mutates: safety.mutates,
        operator: safety.operator,
        dry_run: CommandDryRunMetadata {
            supported: safety.dry_run_flag.is_some(),
            flag: safety.dry_run_flag.map(str::to_string),
        },
        output: CommandOutputMetadata {
            structured: safety.structured_output,
            notes: safety.output_notes.to_string(),
        },
        lab: CommandLabMetadata {
            supported: safety.lab_supported,
            notes: safety.lab_notes.to_string(),
        },
        docs: CommandDocsMetadata {
            path: docs_path(&path, dynamic_commands),
        },
        risk_exemption: safety.risk_exemption.map(str::to_string),
        extension: dynamic_command.and_then(|command| command.extension.clone()),
        dangerous_flags: safety
            .dangerous_flags
            .into_iter()
            .map(str::to_string)
            .collect(),
        subcommands: entry
            .subcommands
            .iter()
            .map(|subcommand| command_safety_entry(subcommand, &path, dynamic_commands))
            .collect(),
    }
}

struct CommandSafetyMetadata {
    mutates: bool,
    operator: bool,
    dry_run_flag: Option<&'static str>,
    structured_output: bool,
    output_notes: &'static str,
    lab_supported: bool,
    lab_notes: &'static str,
    risk_exemption: Option<&'static str>,
    dangerous_flags: Vec<&'static str>,
}

impl Default for CommandSafetyMetadata {
    fn default() -> Self {
        Self {
            mutates: false,
            operator: false,
            dry_run_flag: None,
            structured_output: true,
            output_notes: "standard CLI output contract",
            lab_supported: false,
            lab_notes: "not declared as Lab-routable in the safety manifest",
            risk_exemption: None,
            dangerous_flags: Vec::new(),
        }
    }
}

impl CommandSafetyMetadata {
    fn mutating(&mut self, output_notes: &'static str) {
        self.mutates = true;
        self.output_notes = output_notes;
    }

    fn operator_mutating(&mut self, output_notes: &'static str) {
        self.mutating(output_notes);
        self.operator = true;
    }
}

fn flatten_manifest_entries(entries: &[CommandSafetyEntry]) -> Vec<&CommandSafetyEntry> {
    let mut flattened = Vec::new();

    for entry in entries {
        flattened.push(entry);
        flattened.extend(flatten_manifest_entries(&entry.subcommands));
    }

    flattened
}

fn entry_has_action_metadata(entry: &CommandSafetyEntry) -> bool {
    entry.dry_run.supported
        || !entry.dangerous_flags.is_empty()
        || entry.risk_exemption.is_some()
        || entry.output.notes.contains("--apply")
        || entry.output.notes.contains("--dry-run")
}

fn command_safety_metadata(path: &[String]) -> CommandSafetyMetadata {
    let mut metadata = CommandSafetyMetadata::default();

    if let Some(top_level) = path.first().and_then(|name| registered_command(name)) {
        metadata.output_notes = top_level.output_notes;
        metadata.lab_supported = top_level.lab_supported;
        metadata.lab_notes = top_level.lab_notes;

        if path.len() == 1 {
            metadata.mutates = top_level.safety.mutates;
            metadata.operator = top_level.safety.operator;
            metadata.dry_run_flag = top_level.safety.dry_run_flag;
            metadata.risk_exemption = top_level.safety.risk_exemption;
            metadata.dangerous_flags = top_level.safety.dangerous_flags.to_vec();
        }

        let subcommand_path = path.iter().skip(1).map(String::as_str).collect::<Vec<_>>();
        if let Some(path_safety) = top_level.path_safety(&subcommand_path) {
            metadata.mutates = path_safety.safety.mutates;
            metadata.operator = path_safety.safety.operator;
            metadata.dry_run_flag = path_safety.safety.dry_run_flag;
            metadata.risk_exemption = path_safety.safety.risk_exemption;
            metadata.dangerous_flags = path_safety.safety.dangerous_flags.to_vec();
            if let Some(output_notes) = path_safety.output_notes {
                metadata.output_notes = output_notes;
            }
            if let Some(lab_notes) = path_safety.lab_notes {
                metadata.lab_notes = lab_notes;
            }
            return metadata;
        }
    }

    let path = path.iter().map(String::as_str).collect::<Vec<_>>();
    match path.as_slice() {
        ["worktree", "queue-create"] => {
            metadata.mutating("default output creates task worktrees one-at-a-time; pass --dry-run to plan without creating");
            metadata.dry_run_flag = Some("--dry-run");
        }
        ["worktree", "create"] => {
            metadata.mutating("creates a task worktree from a registered component checkout");
        }
        ["worktree", "remove"] => {
            metadata.mutating("removes a task worktree after safety checks");
            metadata.dangerous_flags = vec!["--force"];
        }
        ["worktree", "cleanup"] => {
            metadata.operator_mutating(
                "default output is a non-mutating task-worktree cleanup plan; pass --apply to remove eligible worktrees, and --cleanup-artifacts to include rebuildable Homeboy artifacts",
            );
            metadata.dry_run_flag = Some("--dry-run");
            metadata.dangerous_flags = vec!["--apply", "--force", "--cleanup-artifacts"];
        }
        ["tunnel", "service", "expose"]
        | ["tunnel", "service", "set"]
        | ["tunnel", "service", "remove"] => {
            metadata.operator_mutating("mutates private service tunnel declarations");
        }
        ["tunnel", "service", "start"] | ["tunnel", "service", "stop"] => {
            metadata.operator_mutating("mutates private service tunnel runtime state");
        }
        ["tunnel", "preview-client", "start"]
        | ["tunnel", "preview-consumer", "run"]
        | ["tunnel", "preview-ingress", "serve"]
        | ["tunnel", "artifact-origin", "serve"] => {
            metadata.operator_mutating("starts or supervises tunnel preview runtime state");
        }
        ["tunnel", "preview-ingress", "route"] | ["tunnel", "preview-ingress", "unroute"] => {
            metadata.operator_mutating("mutates preview ingress route state");
        }
        ["tunnel", "preview-ingress", "install"] => {
            metadata.operator = true;
            metadata.output_notes = "renders a non-destructive operator install plan";
        }
        ["stack", "create"] | ["stack", "add-pr"] | ["stack", "remove-pr"] => {
            metadata.mutating("mutates persisted stack specification metadata");
        }
        ["stack", "apply"] | ["stack", "rebase"] => {
            metadata.operator_mutating("mutates the configured stack target branch");
            metadata.risk_exemption = Some(
                "stack command name is the explicit branch mutation action; status/sync --dry-run are the planning paths",
            );
        }
        ["stack", "sync"] => {
            metadata.operator_mutating("mutates the configured stack target branch and may update the stack spec unless --dry-run is passed");
            metadata.dry_run_flag = Some("--dry-run");
        }
        ["stack", "push"] => {
            metadata.operator_mutating("pushes the configured stack target branch to its remote");
            metadata.risk_exemption = Some(
                "push is the explicit remote publication action; no dry-run contract exists yet",
            );
        }
        _ => {}
    }

    metadata
}

fn docs_path(path: &[String], dynamic_commands: &[DynamicCommandDescriptor]) -> Option<String> {
    if let Some(dynamic) = dynamic_command_for_path(path, dynamic_commands) {
        return dynamic.docs_path.clone();
    }

    let command = path.first()?;

    registered_command(command).and_then(|entry| entry.docs_path())
}

fn dynamic_command_for_path<'a>(
    path: &[String],
    dynamic_commands: &'a [DynamicCommandDescriptor],
) -> Option<&'a DynamicCommandDescriptor> {
    let command = path.first()?;

    if path.len() == 1 {
        dynamic_commands.iter().find(|entry| entry.name == *command)
    } else {
        None
    }
}
