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

    fn guarded_operator_mutating(
        &mut self,
        output_notes: &'static str,
        dangerous_flags: Vec<&'static str>,
    ) {
        self.operator_mutating(output_notes);
        self.dangerous_flags = dangerous_flags;
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
        ["contract", "manifest"] => {}
        ["self", "docs", "map"] => {
            metadata.mutating(
                "default JSON output is non-mutating; pass --write to write markdown docs to disk",
            );
            metadata.dangerous_flags = vec!["--write"];
        }
        ["review", "ci", "autofix"] => {
            metadata.operator_mutating("commits and pushes prepared CI autofix changes");
        }
        ["cleanup", "artifacts"] => {
            metadata.mutating(
                "default output is a non-mutating cleanup plan; pass --apply to remove artifacts",
            );
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["self", "cleanup-runtime-tmp"] => {
            metadata.operator_mutating("default output is a non-mutating cleanup plan; pass --apply to delete runtime temp entries");
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["extension", "setup"]
        | ["extension", "refresh"]
        | ["extension", "relink"]
        | ["extension", "dev-run"]
        | ["extension", "install-for-component"]
        | ["extension", "set"] => {
            metadata.mutating("mutates installed extension files or extension manifest metadata");
        }
        ["extension", "install"] => {
            metadata.mutating("mutates installed extension files or extension manifest metadata");
            metadata.dangerous_flags = vec!["--replace"];
        }
        ["extension", "update"] => {
            metadata.mutating("mutates installed extension files or extension manifest metadata");
            metadata.dangerous_flags = vec!["--force"];
        }
        ["runtime", "refresh"] => {
            metadata.mutating("mutates installed runtime package files");
        }
        ["extension", "uninstall"] => {
            metadata.mutating("mutates installed extension files or extension manifest metadata");
            metadata.dangerous_flags = vec!["uninstall"];
        }
        ["runs", "reconcile"] => {
            metadata.mutating("marks orphaned running records stale unless --dry-run is passed");
            metadata.dry_run_flag = Some("--dry-run");
        }
        ["runs", "import"] => {
            metadata.mutating(
                "imports observation bundle or GitHub Actions artifacts into the local run store",
            );
        }
        ["runs", "loop-sync"] => {
            metadata.mutating(
                "syncs copied loop archives into observation runs/artifacts unless --dry-run is passed",
            );
            metadata.dry_run_flag = Some("--dry-run");
        }
        ["runs", "artifact", "cleanup-downloads"] | ["runs", "artifact", "cleanup-persisted"] => {
            metadata.mutating(
                "default output is a non-mutating cleanup plan; pass --apply to delete artifacts",
            );
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["runs", "resources"] => {
            metadata.mutating("default output is non-mutating; pass --cleanup-plan to plan lifecycle resource cleanup or --apply with --cleanup-root to delete bounded apply-intended candidates");
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["runs", "artifact", "attach"] => {
            metadata.mutating(
                "copies an existing runner-side file into the persisted local artifact store and records it against a run",
            );
        }
        ["agent-task", "promote"] => {
            metadata.mutating(
                "applies a selected patch artifact into a managed worktree unless --dry-run is passed",
            );
            metadata.dry_run_flag = Some("--dry-run");
        }
        ["agent-task", "active"] => {
            metadata.mutating(
                "reads active runs by default; --reconcile cancels stale active records unless --dry-run is passed",
            );
            metadata.dry_run_flag = Some("--dry-run");
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
            metadata.mutating("mutates durable agent-task loop controller state");
        }
        ["agent-task", "auth", "remove"] => {
            metadata.operator_mutating("removes one agent-task provider secret source mapping");
        }
        ["agent-task", "prompts", "remove"] => {
            metadata.mutating("removes one stored agent-task prompt");
        }
        ["agent-task", "fanout", "cook-batch"] => {
            metadata.operator_mutating(
                "creates/reuses task worktrees and can run the generated fanout unless --dry-run is passed",
            );
            metadata.dry_run_flag = Some("--dry-run");
            metadata.dangerous_flags = vec!["--run-plan"];
        }
        ["fuzz", "replay"] | ["fuzz", "minimize"] => {
            metadata.mutating(
                "replays or minimizes a persisted fuzz case against local code and may write run artifacts",
            );
        }
        ["fuzz"] | ["fuzz", "run"] | ["fuzz", "plan"] | ["fuzz", "run-campaign"] => {
            metadata.output_notes = "read-only fuzz planning/execution contract by default; --allow-destructive infers isolated mode and attaches an auditable homeboy/isolation-proof/v1 unless one is supplied";
            metadata.dangerous_flags = vec!["--allow-destructive"];
        }
        ["rig", "release-lock"] => {
            metadata.operator_mutating(
                "releases a local rig active-run lease; --force can reclaim a live holder's guardrail",
            );
            metadata.dangerous_flags = vec!["--force"];
        }
        ["db", "delete-row"] | ["db", "drop-table"] => {
            metadata
                .operator_mutating("default output is a non-mutating plan; pass --apply to mutate");
        }
        ["git", "issue", "create"]
        | ["git", "issue", "comment"]
        | ["git", "issue", "close"]
        | ["git", "issue", "edit"] => {
            metadata
                .operator_mutating("mutates GitHub issue state through the configured repository");
            metadata.risk_exemption = Some(
                "the issue subcommand is the explicit GitHub write action; no dry-run contract exists yet",
            );
        }
        ["git", "pr", "create"]
        | ["git", "pr", "edit"]
        | ["git", "pr", "comment"]
        | ["git", "pr", "refresh"]
        | ["git", "pr", "policy", "open"] => {
            metadata.operator_mutating("mutates GitHub pull request state or branch state");
            metadata.risk_exemption = Some(
                "the PR subcommand is the explicit GitHub write action; no dry-run contract exists yet",
            );
        }
        ["git", "pr", "fleet"] | ["git", "pr", "land"] => {
            metadata.operator_mutating(
                "reports by default or with --dry-run; apply/merge flags mutate PR state",
            );
            metadata.dry_run_flag = Some("--dry-run");
            metadata.dangerous_flags = vec!["--apply", "--delete-branch"];
        }
        ["runs", "findings", "reconcile"] | ["runs", "findings", "reconcile-run"] => {
            metadata.operator_mutating("default output is a non-mutating issue reconciliation plan; pass --apply to mutate tracker state");
            metadata.dry_run_flag = Some("--dry-run");
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["review", "audit", "baseline", "refresh"] | ["review", "audit", "baseline", "merge"] => {
            metadata.mutating("mutates persisted audit baseline data in component configuration");
        }
        ["refactor", "rename"]
        | ["refactor", "add"]
        | ["refactor", "move"]
        | ["refactor", "propagate"]
        | ["refactor", "transform"]
        | ["refactor", "decompose"] => {
            metadata.mutating("reports a plan by default; pass --write to rewrite source files");
            metadata.dangerous_flags = vec!["--write"];
        }
        ["rig", "up"] => {
            metadata.operator_mutating("mutates local rig runtime state unless --dry-run is passed with --runner to emit a runner exec plan");
            metadata.dry_run_flag = Some("--dry-run");
        }
        ["rig", "down"] | ["rig", "repair"] | ["rig", "install"] | ["rig", "update"] => {
            metadata.operator_mutating("mutates local rig runtime state or installed rig packages");
        }
        ["rig", "sync"]
        | ["rig", "app", "install"]
        | ["rig", "app", "update"]
        | ["rig", "app", "uninstall"] => {
            metadata.operator_mutating("mutates rig-managed files unless --dry-run is passed");
            metadata.dry_run_flag = Some("--dry-run");
        }
        ["rig", "sources", "remove"] | ["rig", "sources", "refresh"] => {
            metadata.mutating("mutates installed rig source metadata");
        }
        ["runner", "add"]
        | ["runner", "enable"]
        | ["runner", "set"]
        | ["runner", "trust"]
        | ["runner", "pair"]
        | ["runner", "remove"]
        | ["runner", "disconnect"]
        | ["runner", "refresh-homeboy"] => {
            metadata.operator_mutating(
                "mutates runner configuration, trust policy, or runner lifecycle state",
            );
        }
        ["runner", "connect"] | ["runner", "work"] => {
            metadata.operator_mutating("mutates runner lifecycle state");
            metadata.risk_exemption = Some(
                "runner lifecycle command name is the explicit operator action; no dry-run contract exists yet",
            );
        }
        ["runner", "doctor"] => {
            metadata.operator_mutating(
                "diagnoses runners by default; --repair mutates runner lifecycle state",
            );
            metadata.dangerous_flags = vec!["--repair"];
        }
        ["runner", "exec"] => {
            metadata.operator_mutating("executes commands on a runner unless --dry-run is passed");
            metadata.dry_run_flag = Some("--dry-run");
        }
        ["runner", "lifecycle"] => {
            metadata.output_notes = "non-mutating runner workspace lifecycle/finalization readiness report suitable for RunOutcomeEnvelope embedding";
        }
        ["runner", "workspace", "sync"] => {
            metadata.operator_mutating("materializes a local worktree into runner workspace state");
            metadata.dangerous_flags = vec!["--allow-dirty-lab-workspace"];
        }
        ["runner", "workspace", "update"] => {
            metadata
                .operator_mutating("advances a prepared runner workspace from its snapshot lease");
        }
        ["runner", "workspace", "pull"] => {
            metadata.operator_mutating(
                "copies selected files from runner workspace state to a local destination",
            );
            metadata.dry_run_flag = Some("--dry-run");
        }
        ["runner", "workspace", "apply"] => {
            metadata
                .operator_mutating("applies a Lab-generated workspace patch to a local worktree");
            metadata.dangerous_flags = vec!["--force"];
        }
        ["runner", "workspace", "prune"] => {
            metadata.operator_mutating("default output is a non-mutating orphan cleanup plan with candidate/remaining bytes; pass --apply to delete exact runner workspace paths and --passes to drain bounded pages");
            metadata.dangerous_flags = vec!["--apply"];
        }
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
            metadata.mutating(
                "removes cleanup-eligible task worktrees; pass --cleanup-artifacts to also remove rebuildable Homeboy artifacts",
            );
            metadata.dry_run_flag = Some("--dry-run");
            metadata.dangerous_flags = vec!["--force", "--cleanup-artifacts"];
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
        ["extension", "run"] | ["extension", "exec"] => {
            metadata.guarded_operator_mutating(
                "executes extension-owned runtime commands with forwarded arguments that may mutate the target system",
                vec!["extension runtime command", "passthrough args"],
            );
        }
        ["extension", "action"] => {
            metadata.guarded_operator_mutating(
                "executes extension-owned actions that may mutate the target system",
                vec!["extension action"],
            );
        }
        ["refactor", "undo", "delete"] => {
            metadata.mutating("deletes an undo snapshot without restoring it");
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
