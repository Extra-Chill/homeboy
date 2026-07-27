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
//!
//! Safety metadata itself is declared once, in
//! [`crate::command_contract::COMMAND_SPECS`]: top-level `CommandSafetySpec`
//! plus a `CommandPathSafetySpec` table per command family. This module only
//! resolves those declarations against the live clap surface, so a declared
//! path that does not exist in clap is a bug the registry guard test catches
//! rather than a silently ignored entry.

use crate::cli_surface::{
    current_command_surface, CommandDocsMetadata, CommandDryRunMetadata, CommandLabMetadata,
    CommandOutputMetadata, CommandSafetyAuditFinding, CommandSafetyAuditReport, CommandSafetyEntry,
    CommandSafetyManifest, CommandSurface, CommandSurfaceEntry, DynamicCommandDescriptor,
};
use crate::command_contract::{registered_command, CommandSafetySpec};

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
    let mut resolved = resolved_command_safety(&path);
    let dynamic_command = dynamic_command_for_path(&path, dynamic_commands);

    if let Some(dynamic_safety) = dynamic_command.and_then(|command| command.safety.as_ref()) {
        resolved.safety.mutates = dynamic_safety.mutates;
        resolved.safety.operator = dynamic_safety.operator;
        resolved.output_notes = dynamic_safety.output_notes;
        resolved.lab_notes = dynamic_safety.lab_notes;
        resolved.dangerous_flags = dynamic_safety.dangerous_flags.clone();
    }

    CommandSafetyEntry {
        name: entry.name.clone(),
        aliases: entry.visible_aliases.clone(),
        hidden: entry.hidden,
        path: path.clone(),
        mutates: resolved.safety.mutates,
        operator: resolved.safety.operator,
        dry_run: CommandDryRunMetadata {
            supported: resolved.safety.dry_run_flag.is_some(),
            flag: resolved.safety.dry_run_flag.map(str::to_string),
        },
        output: CommandOutputMetadata {
            structured: true,
            notes: resolved.output_notes.to_string(),
        },
        lab: CommandLabMetadata {
            supported: resolved.lab_supported,
            notes: resolved.lab_notes.to_string(),
        },
        docs: CommandDocsMetadata {
            path: docs_path(&path, dynamic_commands),
        },
        risk_exemption: resolved.safety.risk_exemption.map(str::to_string),
        extension: dynamic_command.and_then(|command| command.extension.clone()),
        dangerous_flags: resolved
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

/// Output/Lab notes for command paths with no `COMMAND_SPECS` entry, i.e.
/// dynamic extension commands.
const DEFAULT_OUTPUT_NOTES: &str = "standard CLI output contract";
const DEFAULT_LAB_NOTES: &str = "not declared as Lab-routable in the safety manifest";

/// Command safety resolved from the declarative registry for one clap path.
struct ResolvedCommandSafety {
    safety: CommandSafetySpec,
    output_notes: &'static str,
    lab_supported: bool,
    lab_notes: &'static str,
    /// Owned so dynamic (extension) commands can replace the declared flags.
    dangerous_flags: Vec<&'static str>,
}

/// Resolves one clap path against the declarative command registry: top-level
/// command metadata first, then the owning command's `CommandPathSafetySpec`
/// table. There is no second, imperative registry — a command path that wants
/// safety metadata declares it in `COMMAND_SPECS`.
fn resolved_command_safety(path: &[String]) -> ResolvedCommandSafety {
    let mut resolved = ResolvedCommandSafety {
        safety: CommandSafetySpec::read_only(),
        output_notes: DEFAULT_OUTPUT_NOTES,
        lab_supported: false,
        lab_notes: DEFAULT_LAB_NOTES,
        dangerous_flags: Vec::new(),
    };

    let Some(top_level) = path.first().and_then(|name| registered_command(name)) else {
        return resolved;
    };

    resolved.output_notes = top_level.output_notes;
    resolved.lab_supported = top_level.lab_supported;
    resolved.lab_notes = top_level.lab_notes;

    if path.len() == 1 {
        resolved.safety = top_level.safety;
    }

    let subcommand_path = path.iter().skip(1).map(String::as_str).collect::<Vec<_>>();
    if let Some(path_safety) = top_level.path_safety(&subcommand_path) {
        resolved.safety = path_safety.safety;

        if let Some(output_notes) = path_safety.output_notes {
            resolved.output_notes = output_notes;
        }
        if let Some(lab_notes) = path_safety.lab_notes {
            resolved.lab_notes = lab_notes;
        }
    }

    resolved.dangerous_flags = resolved.safety.dangerous_flags.to_vec();

    resolved
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
