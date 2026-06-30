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
        metadata.structured_output =
            top_level.json_family != crate::command_contract::CommandJsonFamily::RawOnly;
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
    }

    let path = path.iter().map(String::as_str).collect::<Vec<_>>();
    match path.as_slice() {
        ["manifest"] => {}
        ["docs", "map"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "default JSON output is non-mutating; pass --write to write markdown docs to disk";
            metadata.dangerous_flags = vec!["--write"];
        }
        ["deps", "install"] | ["deps", "update"] | ["deps", "stack", "apply"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates dependency manifests, lockfiles, or installed dependency trees";
        }
        ["ci", "autofix"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "commits and pushes prepared CI autofix changes";
        }
        ["cleanup", "artifacts"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "default output is a non-mutating cleanup plan; pass --apply to remove artifacts";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["self", "cleanup-runtime-tmp"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "default output is a non-mutating cleanup plan; pass --apply to delete runtime temp entries";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["config", "set"] | ["config", "remove"] | ["config", "reset"] => {
            metadata.mutates = true;
        }
        ["project", "create"]
        | ["project", "set"]
        | ["project", "remove"]
        | ["project", "rename"]
        | ["project", "delete"]
        | ["project", "init"]
        | ["project", "components", "set"]
        | ["project", "components", "attach-path"]
        | ["project", "components", "remove"]
        | ["project", "components", "clear"]
        | ["project", "pin", "add"]
        | ["project", "pin", "remove"]
        | ["project", "pin", "rename"]
        | ["project", "pin", "update"] => {
            metadata.mutates = true;
        }
        ["component", "create"]
        | ["component", "set"]
        | ["component", "delete"]
        | ["component", "rename"]
        | ["component", "setup"] => {
            metadata.mutates = true;
        }
        ["component", "reconcile"] | ["component", "artifacts"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "default output is non-mutating; pass --apply to repair or remove artifacts";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["server", "create"]
        | ["server", "set"]
        | ["server", "delete"]
        | ["server", "connect"]
        | ["server", "disconnect"]
        | ["server", "key", "generate"]
        | ["server", "key", "import"]
        | ["server", "key", "use"]
        | ["server", "key", "unset"] => {
            metadata.mutates = true;
            metadata.operator = true;
        }
        ["extension", "setup"]
        | ["extension", "refresh"]
        | ["extension", "relink"]
        | ["extension", "install-for-component"]
        | ["extension", "set"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates installed extension files or extension manifest metadata";
        }
        ["extension", "install"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates installed extension files or extension manifest metadata";
            metadata.dangerous_flags = vec!["--replace"];
        }
        ["extension", "update"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates installed extension files or extension manifest metadata";
            metadata.dangerous_flags = vec!["--force"];
        }
        ["runtime", "refresh"] => {
            metadata.mutates = true;
            metadata.output_notes = "mutates installed runtime package files";
        }
        ["extension", "uninstall"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates installed extension files or extension manifest metadata";
            metadata.dangerous_flags = vec!["uninstall"];
        }
        ["runs", "reconcile"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "marks orphaned running records stale unless --dry-run is passed";
        }
        ["runs", "import"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "imports observation bundle or GitHub Actions artifacts into the local run store";
        }
        ["runs", "loop-sync"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "syncs copied loop archives into observation runs/artifacts unless --dry-run is passed";
        }
        ["runs", "artifact", "cleanup-downloads"] | ["runs", "artifact", "cleanup-persisted"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "default output is a non-mutating cleanup plan; pass --apply to delete artifacts";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["runs", "artifact", "attach"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "copies an existing runner-side file into the persisted local artifact store and records it against a run";
        }
        ["agent-task", "promote"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "applies a selected patch artifact into a managed worktree unless --dry-run is passed";
        }
        ["agent-task", "active"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "reads active runs by default; --reconcile cancels stale active records unless --dry-run is passed";
        }
        ["agent-task", "controller", "init"]
        | ["agent-task", "controller", "from-spec"]
        | ["agent-task", "controller", "run-from-spec"]
        | ["agent-task", "controller", "materialize"]
        | ["agent-task", "controller", "events"]
        | ["agent-task", "controller", "apply-event"]
        | ["agent-task", "controller", "run-next"]
        | ["agent-task", "controller", "run"]
        | ["agent-task", "controller", "resume"]
        | ["agent-task", "controller", "mark-human-ready"] => {
            metadata.mutates = true;
            metadata.output_notes = "mutates durable agent-task loop controller state";
        }
        ["agent-task", "auth", "remove"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "removes one agent-task provider secret source mapping";
        }
        ["agent-task", "prompts", "remove"] => {
            metadata.mutates = true;
            metadata.output_notes = "removes one stored agent-task prompt";
        }
        ["agent-task", "fanout", "cook-batch"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "creates/reuses DMC worktrees and can run the generated fanout unless --dry-run is passed";
            metadata.dangerous_flags = vec!["--run-plan"];
        }
        ["fuzz", "replay"] | ["fuzz", "minimize"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "replays or minimizes a persisted fuzz case against local code and may write run artifacts";
        }
        ["fuzz"] | ["fuzz", "run"] | ["fuzz", "plan"] => {
            metadata.output_notes = "read-only fuzz planning/execution contract by default; --allow-destructive requires explicit disposable homeboy/isolation-proof/v1 input";
            metadata.dangerous_flags = vec!["--allow-destructive"];
        }
        ["rig", "release-lock"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "releases a local rig active-run lease; --force can reclaim a live holder's guardrail";
            metadata.dangerous_flags = vec!["--force"];
        }
        ["db", "delete-row"] | ["db", "drop-table"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "default output is a non-mutating plan; pass --apply to mutate";
        }
        ["file", "write"] | ["file", "delete"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "default output is a non-mutating plan; pass --apply to mutate";
        }
        ["file", "copy"]
        | ["file", "edit"]
        | ["file", "mkdir"]
        | ["file", "rename"]
        | ["file", "sync"] => {
            metadata.mutates = true;
        }
        ["fleet", "exec"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--check");
            metadata.output_notes = "default output is blocked for remote execution; pass --check to plan or --apply to execute";
            metadata.dangerous_flags = vec!["--apply"];
            metadata.lab_notes = "local-only: depends on local fleet/project/server configuration before SSH fan-out";
        }
        ["fleet", "create"]
        | ["fleet", "set"]
        | ["fleet", "delete"]
        | ["fleet", "add"]
        | ["fleet", "remove"] => {
            metadata.mutates = true;
            metadata.output_notes = "mutates local fleet configuration";
        }
        ["api", "post"] | ["api", "put"] | ["api", "patch"] | ["api", "delete"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutating API requests require --apply";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["git", "issue", "create"]
        | ["git", "issue", "comment"]
        | ["git", "issue", "close"]
        | ["git", "issue", "edit"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates GitHub issue state through the configured repository";
            metadata.risk_exemption = Some(
                "the issue subcommand is the explicit GitHub write action; no dry-run contract exists yet",
            );
        }
        ["git", "pr", "create"]
        | ["git", "pr", "edit"]
        | ["git", "pr", "comment"]
        | ["git", "pr", "refresh"]
        | ["git", "pr", "policy", "open"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates GitHub pull request state or branch state";
            metadata.risk_exemption = Some(
                "the PR subcommand is the explicit GitHub write action; no dry-run contract exists yet",
            );
        }
        ["git", "pr", "fleet"] | ["git", "pr", "land"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "reports by default or with --dry-run; apply/merge flags mutate PR state";
            metadata.dangerous_flags = vec!["--apply", "--delete-branch"];
        }
        ["issues", "reconcile"] | ["issues", "reconcile-run"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "default output is a non-mutating issue reconciliation plan; pass --apply to mutate tracker state";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["audit-baseline", "refresh"] | ["audit-baseline", "merge"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates persisted audit baseline data in component configuration";
        }
        ["refactor", "rename"]
        | ["refactor", "add"]
        | ["refactor", "move"]
        | ["refactor", "propagate"]
        | ["refactor", "transform"]
        | ["refactor", "decompose"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "reports a plan by default; pass --write to rewrite source files";
            metadata.dangerous_flags = vec!["--write"];
        }
        ["rig", "up"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "mutates local rig runtime state unless --dry-run is passed with --runner to emit a runner exec plan";
        }
        ["rig", "down"] | ["rig", "repair"] | ["rig", "install"] | ["rig", "update"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates local rig runtime state or installed rig packages";
        }
        ["rig", "sync"]
        | ["rig", "app", "install"]
        | ["rig", "app", "update"]
        | ["rig", "app", "uninstall"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "mutates rig-managed files unless --dry-run is passed";
        }
        ["rig", "sources", "remove"] | ["rig", "sources", "refresh"] => {
            metadata.mutates = true;
            metadata.output_notes = "mutates installed rig source metadata";
        }
        ["runner", "add"]
        | ["runner", "enable"]
        | ["runner", "set"]
        | ["runner", "trust"]
        | ["runner", "pair"]
        | ["runner", "remove"]
        | ["runner", "disconnect"]
        | ["runner", "refresh-homeboy"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes =
                "mutates runner configuration, trust policy, or runner lifecycle state";
        }
        ["runner", "connect"] | ["runner", "work"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates runner lifecycle state";
            metadata.risk_exemption = Some(
                "runner lifecycle command name is the explicit operator action; no dry-run contract exists yet",
            );
        }
        ["runner", "doctor"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes =
                "diagnoses runners by default; --repair mutates runner lifecycle state";
            metadata.dangerous_flags = vec!["--repair"];
        }
        ["runner", "exec"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "executes commands on a runner unless --dry-run is passed";
        }
        ["runner", "workspace", "sync"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "materializes a local worktree into runner workspace state";
            metadata.dangerous_flags = vec!["--allow-dirty-lab-workspace"];
        }
        ["runner", "workspace", "pull"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "copies selected files from runner workspace state to a local destination";
        }
        ["runner", "workspace", "apply"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "applies a Lab-generated workspace patch to a local worktree";
            metadata.dangerous_flags = vec!["--force"];
        }
        ["runner", "workspace", "prune"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "default output is a non-mutating orphan cleanup plan; pass --apply to delete exact runner workspace paths";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["http", "request"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutating HTTP methods require --apply; GET, HEAD, and OPTIONS are allowed without it";
            metadata.dangerous_flags =
                vec!["--apply", "METHOD!=GET", "METHOD!=HEAD", "METHOD!=OPTIONS"];
        }
        ["worktree", "queue-create"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "default output creates DMC worktrees one-at-a-time; pass --dry-run to plan without creating";
        }
        ["worktree", "create"] => {
            metadata.mutates = true;
            metadata.output_notes = "creates a task worktree from a registered component checkout";
        }
        ["worktree", "remove"] => {
            metadata.mutates = true;
            metadata.output_notes = "removes a task worktree after safety checks";
            metadata.dangerous_flags = vec!["--force"];
        }
        ["worktree", "cleanup"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "removes cleanup-eligible task worktrees and rebuildable artifacts";
            metadata.dangerous_flags = vec!["--force"];
        }
        ["tunnel", "service", "expose"]
        | ["tunnel", "service", "set"]
        | ["tunnel", "service", "remove"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates private service tunnel declarations";
        }
        ["tunnel", "service", "start"] | ["tunnel", "service", "stop"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates private service tunnel runtime state";
        }
        ["tunnel", "preview-client", "start"]
        | ["tunnel", "preview-consumer", "run"]
        | ["tunnel", "preview-ingress", "serve"]
        | ["tunnel", "artifact-origin", "serve"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "starts or supervises tunnel preview runtime state";
        }
        ["tunnel", "preview-ingress", "route"] | ["tunnel", "preview-ingress", "unroute"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates preview ingress route state";
        }
        ["tunnel", "preview-ingress", "install"] => {
            metadata.operator = true;
            metadata.output_notes = "renders a non-destructive operator install plan";
        }
        ["stack", "create"] | ["stack", "add-pr"] | ["stack", "remove-pr"] => {
            metadata.mutates = true;
            metadata.output_notes = "mutates persisted stack specification metadata";
        }
        ["stack", "apply"] | ["stack", "rebase"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates the configured stack target branch";
            metadata.risk_exemption = Some(
                "stack command name is the explicit branch mutation action; status/sync --dry-run are the planning paths",
            );
        }
        ["stack", "sync"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "mutates the configured stack target branch and may update the stack spec unless --dry-run is passed";
        }
        ["stack", "push"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "pushes the configured stack target branch to its remote";
            metadata.risk_exemption = Some(
                "push is the explicit remote publication action; no dry-run contract exists yet",
            );
        }
        ["extension", "run"] | ["extension", "exec"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "executes extension-owned runtime commands with forwarded arguments that may mutate the target system";
            metadata.dangerous_flags = vec!["extension runtime command", "passthrough args"];
        }
        ["extension", "action"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes =
                "executes extension-owned actions that may mutate the target system";
            metadata.dangerous_flags = vec!["extension action"];
        }
        ["undo", "delete"] => {
            metadata.mutates = true;
            metadata.output_notes = "deletes an undo snapshot without restoring it";
        }
        ["auth", "login"]
        | ["auth", "set"]
        | ["auth", "remove"]
        | ["auth", "logout"]
        | ["auth", "profile", "set-basic"]
        | ["auth", "profile", "set-bearer"]
        | ["auth", "profile", "remove"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates keychain-backed authentication state";
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{command_surface_from, current_command_surface, Cli};
    use clap::CommandFactory;

    fn manifest_path<'a>(
        manifest: &'a CommandSafetyManifest,
        path: &[&str],
    ) -> &'a CommandSafetyEntry {
        manifest
            .find_path(path)
            .unwrap_or_else(|| panic!("missing safety entry for {path:?}"))
    }

    fn flatten_manifest_entries<'a>(
        entries: &'a [CommandSafetyEntry],
        flattened: &mut Vec<&'a CommandSafetyEntry>,
    ) {
        for entry in entries {
            flattened.push(entry);
            flatten_manifest_entries(&entry.subcommands, flattened);
        }
    }

    fn command_has_visible_flag(command: &clap::Command, flag: &str) -> bool {
        command
            .get_arguments()
            .any(|arg| !arg.is_hide_set() && arg.get_long().is_some_and(|long| long == flag))
    }

    fn command_has_visible_risk_flag(command: &clap::Command) -> bool {
        ["apply", "dry-run", "force"]
            .iter()
            .any(|flag| command_has_visible_flag(command, flag))
    }

    fn path_has_visible_risk_flag(command: &clap::Command, path: &[String]) -> bool {
        let Some((first, rest)) = path.split_first() else {
            return false;
        };
        let Some(subcommand) = command.find_subcommand(first) else {
            return false;
        };
        if rest.is_empty() {
            return command_has_visible_risk_flag(subcommand);
        }
        path_has_visible_risk_flag(subcommand, rest)
    }

    fn is_suspicious_path(entry: &CommandSafetyEntry) -> bool {
        let Some(name) = entry.path.last().map(String::as_str) else {
            return false;
        };

        matches!(
            name,
            "action"
                | "apply"
                | "cleanup"
                | "cleanup-downloads"
                | "cleanup-persisted"
                | "connect"
                | "create"
                | "delete"
                | "disconnect"
                | "exec"
                | "generate"
                | "import"
                | "init"
                | "install"
                | "install-for-component"
                | "refresh"
                | "relink"
                | "release"
                | "remove"
                | "rename"
                | "reset"
                | "set"
                | "uninstall"
                | "unset"
                | "update"
                | "upgrade"
                | "use"
        )
    }

    fn entry_has_safety_classification(entry: &CommandSafetyEntry) -> bool {
        entry.mutates
            || entry.operator
            || entry.dry_run.supported
            || !entry.dangerous_flags.is_empty()
            || entry.risk_exemption.is_some()
            || entry.output.notes.contains("--apply")
            || entry.output.notes.contains("--dry-run")
    }

    fn entry_has_action_metadata(entry: &CommandSafetyEntry) -> bool {
        entry.dry_run.supported
            || !entry.dangerous_flags.is_empty()
            || entry.risk_exemption.is_some()
            || entry.output.notes.contains("--apply")
            || entry.output.notes.contains("--dry-run")
    }

    #[test]
    fn command_safety_manifest_covers_surface_paths() {
        let manifest = current_command_safety_manifest();

        assert!(manifest.find_path(&["db"]).is_some());
        assert!(manifest.find_path(&["db", "delete-row"]).is_some());
        assert!(manifest.find_path(&["file", "write"]).is_some());
        assert!(manifest.find_path(&["api", "post"]).is_some());
        assert!(manifest
            .find_path(&["agent-task", "controller", "run-next"])
            .is_some());
    }

    #[test]
    fn command_safety_manifest_records_clap_visibility_metadata() {
        let manifest = current_command_safety_manifest();

        let command_manifest = manifest_path(&manifest, &["manifest"]);
        assert!(!command_manifest.hidden);
        assert!(command_manifest.output.structured);
        assert!(command_manifest
            .output
            .notes
            .contains("recursive command safety"));

        let visible_status = manifest.find_path(&["status"]).unwrap();
        assert!(!visible_status.hidden);
        assert!(visible_status.aliases.is_empty());
    }

    #[test]
    fn command_safety_manifest_uses_registry_metadata() {
        let manifest = current_command_safety_manifest();

        let bench = manifest.find_path(&["bench"]).unwrap();
        assert!(bench.output.structured);
        assert!(bench.lab.supported);
        assert!(bench.lab.notes.contains("portable Lab offload"));
        assert_eq!(bench.docs.path.as_deref(), Some("docs/commands/bench.md"));
    }

    #[test]
    fn command_safety_manifest_includes_dynamic_command_descriptors() {
        let dynamic_command = DynamicCommandDescriptor::extension_command(
            "ext-tool".to_string(),
            "Run extension tool commands".to_string(),
        );
        let command = Cli::command().subcommand(clap::Command::new("ext-tool"));
        let manifest = command_safety_manifest_from_dynamic(
            command_surface_from(command),
            std::slice::from_ref(&dynamic_command),
        );

        let ext_tool = manifest.find_path(&["ext-tool"]).unwrap();
        assert_eq!(
            ext_tool.docs.path.as_deref(),
            Some("docs/commands/ext-tool.md")
        );
        assert!(ext_tool.output.structured);
    }

    #[test]
    fn command_safety_manifest_classifies_known_dangerous_paths() {
        let manifest = current_command_safety_manifest();

        for path in [
            ["release"].as_slice(),
            ["upgrade"].as_slice(),
            ["cleanup", "artifacts"].as_slice(),
            ["self", "cleanup-runtime-tmp"].as_slice(),
            ["db", "delete-row"].as_slice(),
            ["db", "drop-table"].as_slice(),
            ["file", "write"].as_slice(),
            ["file", "delete"].as_slice(),
            ["docs", "map"].as_slice(),
            ["runs", "reconcile"].as_slice(),
            ["runs", "import"].as_slice(),
            ["runs", "artifact", "cleanup-downloads"].as_slice(),
            ["runs", "artifact", "cleanup-persisted"].as_slice(),
            ["agent-task", "promote"].as_slice(),
            ["extension", "install"].as_slice(),
            ["extension", "update"].as_slice(),
            ["extension", "uninstall"].as_slice(),
            ["extension", "run"].as_slice(),
            ["extension", "action"].as_slice(),
            ["extension", "exec"].as_slice(),
            ["config", "set"].as_slice(),
            ["config", "remove"].as_slice(),
            ["project", "set"].as_slice(),
            ["project", "delete"].as_slice(),
            ["component", "set"].as_slice(),
            ["component", "delete"].as_slice(),
            ["server", "set"].as_slice(),
            ["server", "delete"].as_slice(),
            ["api", "post"].as_slice(),
            ["api", "put"].as_slice(),
            ["api", "patch"].as_slice(),
            ["api", "delete"].as_slice(),
            ["http", "request"].as_slice(),
            ["git", "issue", "create"].as_slice(),
            ["git", "issue", "comment"].as_slice(),
            ["git", "issue", "close"].as_slice(),
            ["git", "issue", "edit"].as_slice(),
            ["git", "pr", "create"].as_slice(),
            ["git", "pr", "edit"].as_slice(),
            ["git", "pr", "comment"].as_slice(),
            ["git", "pr", "refresh"].as_slice(),
            ["git", "pr", "policy", "open"].as_slice(),
            ["runner", "connect"].as_slice(),
            ["runner", "work"].as_slice(),
            ["stack", "apply"].as_slice(),
            ["stack", "rebase"].as_slice(),
            ["stack", "push"].as_slice(),
        ] {
            let entry = manifest_path(&manifest, path);

            assert!(entry.mutates, "{path:?} should be marked mutating");
        }

        for path in [
            ["release"].as_slice(),
            ["upgrade"].as_slice(),
            ["self", "cleanup-runtime-tmp"].as_slice(),
            ["db", "delete-row"].as_slice(),
            ["file", "delete"].as_slice(),
            ["server", "set"].as_slice(),
            ["api", "post"].as_slice(),
            ["http", "request"].as_slice(),
        ] {
            let entry = manifest_path(&manifest, path);
            assert!(entry.operator, "{path:?} should be marked operator-gated");
        }
    }

    #[test]
    fn command_safety_manifest_records_guard_flags_and_dry_run_flags() {
        let manifest = current_command_safety_manifest();

        let deploy = manifest.find_path(&["deploy"]).unwrap();
        assert!(deploy.operator);
        assert_eq!(deploy.dry_run.flag.as_deref(), Some("--dry-run"));
        assert_eq!(deploy.dangerous_flags, vec!["--head", "--force"]);

        let triage = manifest.find_path(&["triage"]).unwrap();
        assert_eq!(triage.dangerous_flags, vec!["--auto-merge"]);

        let docs_map = manifest.find_path(&["docs", "map"]).unwrap();
        assert!(docs_map.output.notes.contains("--write"));
        assert_eq!(docs_map.dangerous_flags, vec!["--write"]);

        let fleet_exec = manifest.find_path(&["fleet", "exec"]).unwrap();
        assert_eq!(fleet_exec.dry_run.flag.as_deref(), Some("--check"));
        assert!(fleet_exec.output.notes.contains("--apply"));
        assert!(fleet_exec.dangerous_flags.contains(&"--apply".to_string()));
        assert!(fleet_exec.lab.notes.contains("local-only"));

        let db_delete_row = manifest.find_path(&["db", "delete-row"]).unwrap();
        assert!(db_delete_row.output.notes.contains("--apply"));

        let file_write = manifest.find_path(&["file", "write"]).unwrap();
        assert!(file_write.output.notes.contains("--apply"));

        let api_post = manifest.find_path(&["api", "post"]).unwrap();
        assert!(api_post.output.notes.contains("--apply"));
        assert!(api_post.dangerous_flags.contains(&"--apply".to_string()));

        let http_request = manifest.find_path(&["http", "request"]).unwrap();
        assert!(http_request.output.notes.contains("--apply"));
        assert!(http_request
            .dangerous_flags
            .contains(&"METHOD!=GET".to_string()));

        let release = manifest_path(&manifest, &["release"]);
        assert_eq!(release.dry_run.flag.as_deref(), Some("--dry-run"));
        assert!(release.dangerous_flags.contains(&"--apply".to_string()));
        assert!(release.dangerous_flags.contains(&"--head".to_string()));

        let cleanup_artifacts = manifest_path(&manifest, &["cleanup", "artifacts"]);
        assert!(cleanup_artifacts.output.notes.contains("--apply"));

        let self_cleanup = manifest_path(&manifest, &["self", "cleanup-runtime-tmp"]);
        assert!(self_cleanup.output.notes.contains("--apply"));

        let runs_reconcile = manifest_path(&manifest, &["runs", "reconcile"]);
        assert_eq!(runs_reconcile.dry_run.flag.as_deref(), Some("--dry-run"));

        let runs_cleanup = manifest_path(&manifest, &["runs", "artifact", "cleanup-persisted"]);
        assert!(runs_cleanup.output.notes.contains("--apply"));

        let agent_task_promote = manifest_path(&manifest, &["agent-task", "promote"]);
        assert_eq!(
            agent_task_promote.dry_run.flag.as_deref(),
            Some("--dry-run")
        );

        let extension_run = manifest_path(&manifest, &["extension", "run"]);
        assert!(extension_run.operator);
        assert!(extension_run
            .dangerous_flags
            .contains(&"passthrough args".to_string()));

        let stack_push = manifest_path(&manifest, &["stack", "push"]);
        assert!(stack_push.risk_exemption.is_some());
    }

    #[test]
    fn manifest_audit_reports_mutating_commands_without_action_metadata() {
        let manifest = current_command_safety_manifest();
        let report = command_safety_manifest_audit(&manifest);

        assert!(report.report_only);

        let findings = report
            .missing_action_metadata
            .iter()
            .map(|finding| finding.path.join(" "))
            .collect::<Vec<_>>();
        assert!(findings.contains(&"config set".to_string()));
        assert!(findings.contains(&"project set".to_string()));
    }

    #[test]
    fn high_risk_action_commands_have_explicit_action_metadata() {
        let manifest = current_command_safety_manifest();

        for path in [
            ["git", "issue", "create"].as_slice(),
            ["git", "issue", "comment"].as_slice(),
            ["git", "issue", "close"].as_slice(),
            ["git", "issue", "edit"].as_slice(),
            ["git", "pr", "create"].as_slice(),
            ["git", "pr", "edit"].as_slice(),
            ["git", "pr", "comment"].as_slice(),
            ["git", "pr", "refresh"].as_slice(),
            ["git", "pr", "policy", "open"].as_slice(),
            ["stack", "apply"].as_slice(),
            ["stack", "rebase"].as_slice(),
            ["stack", "push"].as_slice(),
            ["runner", "connect"].as_slice(),
            ["runner", "work"].as_slice(),
            ["extension", "run"].as_slice(),
            ["extension", "action"].as_slice(),
            ["extension", "exec"].as_slice(),
        ] {
            let entry = manifest_path(&manifest, path);
            assert!(entry.mutates, "{path:?} should be marked mutating");
            assert!(
                entry_has_action_metadata(entry),
                "{path:?} should have dry-run, dangerous/apply, or risk exemption metadata"
            );
        }
    }

    #[test]
    fn suspicious_command_paths_require_safety_classification() {
        let manifest = current_command_safety_manifest();
        let command = Cli::command();
        let mut entries = Vec::new();
        flatten_manifest_entries(&manifest.commands, &mut entries);

        for entry in entries {
            let suspicious = !entry.hidden
                && (is_suspicious_path(entry) || path_has_visible_risk_flag(&command, &entry.path));
            if suspicious {
                assert!(
                    entry_has_safety_classification(entry),
                    "suspicious command path {:?} lacks explicit safety metadata",
                    entry.path
                );
            }
        }
    }

    #[test]
    fn current_command_safety_manifest_matches_surface_derivation() {
        let derived = command_safety_manifest_from(current_command_surface());
        let current = current_command_safety_manifest();
        assert_eq!(derived.commands.len(), current.commands.len());
    }
}
