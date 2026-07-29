//! Cleanup command parsing and canonical operator-command rendering.
//!
//! This is a command-surface contract, not a cleanup implementation. Keeping it
//! here lets parser and reference tests compile without execution adapters.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Args, Debug, PartialEq, Eq)]
pub struct CleanupArgs {
    /// Apply cleanup across the selected categories. Omit for inventory dry-run output.
    #[arg(long)]
    pub apply: bool,

    /// Include only these cleanup categories. Comma-separated or repeatable.
    /// `runner-downloads` is opt-in only: it holds artifacts an operator asked
    /// Homeboy to fetch, so a bare sweep never includes it.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub include: Vec<CleanupCategoryArg>,

    /// Exclude these cleanup categories. Comma-separated or repeatable.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub exclude: Vec<CleanupCategoryArg>,

    /// Override the configured terminal-run retention window for this invocation.
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<i64>,

    /// Override the age floor for metadata-backed runtime temp entries only.
    /// Unmanaged entries retain the configured runtime temp age floor.
    #[arg(long, value_name = "DAYS")]
    pub runtime_tmp_managed_older_than_days: Option<u64>,

    /// Override the configured maximum number of persisted artifacts inspected.
    #[arg(long, value_name = "N")]
    pub limit: Option<i64>,

    /// Include every controller-scratch candidate and retained-resource detail.
    /// Default output keeps representative detail within the shared response budget.
    #[arg(long)]
    pub full: bool,

    /// Continue a bounded shared-store cleanup inventory from this cursor.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,

    #[command(subcommand)]
    pub command: Option<CleanupCommand>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CleanupCategoryArg {
    RepoArtifacts,
    TaskWorktrees,
    WorktreeProviders,
    TerminalRuns,
    PersistedRunArtifacts,
    OrphanedArtifactBytes,
    RunnerDownloads,
    RunnerBinaryCaches,
    RemoteLabWorkspaces,
    RuntimeTmp,
    ControllerScratch,
    SharedCargoTargets,
    ControllerRuntimes,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum CleanupCommand {
    /// Inspect or remove declared reconstructable artifacts across repo worktrees
    Artifacts(CleanupArtifactsArgs),
    /// Aggregate cleanup across configured external worktree providers
    Worktrees(CleanupWorktreesArgs),
    /// Explain retained Homeboy storage without deleting or reconciling resources.
    ///
    /// Reports lifecycle aggregates alongside root filesystem accounting, top-level
    /// stores, largest child paths, ownership classification, and cleanup guidance.
    RetainedStorage(CleanupRetainedStorageArgs),
    /// Run one configured, bounded retention pass
    AutomaticRetention,
}

#[derive(Args, Debug, PartialEq, Eq)]
pub struct CleanupRetainedStorageArgs {
    /// Maximum largest-byte examples to return. The report always aggregates all inspected sources.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Continue largest-byte examples after this deterministic reference token.
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Args, Debug, PartialEq, Eq)]
pub struct CleanupArtifactsArgs {
    /// Apply cleanup. Omit for dry-run output.
    #[arg(long)]
    pub apply: bool,
    /// Clean artifacts from the Homeboy source checkout that built this binary.
    #[arg(long = "self", conflicts_with = "path")]
    pub self_artifacts: bool,
    /// Resolve managed worktrees from this checkout instead of the current directory.
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Also scan this temp root for detached Homeboy build artifacts. Repeatable.
    #[arg(long, value_name = "PATH")]
    pub temp_root: Vec<PathBuf>,
    /// Sort artifact candidates before reporting or applying cleanup.
    #[arg(long, value_enum, default_value = "discovery")]
    pub sort: CleanupArtifactsSortArg,
    /// Limit artifact candidates reported or removed after sorting.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
    /// Only reclaim artifacts from worktrees whose branch is already merged
    /// into its upstream. Preserves in-progress cooks' build dirs.
    #[arg(long)]
    pub merged_only: bool,
    /// Only reclaim artifacts untouched for at least this many days. Composes
    /// with any age floor a declaration owner sets; the stricter one wins.
    #[arg(long, value_name = "DAYS")]
    pub min_age_days: Option<u64>,
    /// Also reclaim extension-declared artifacts from checkouts registered as
    /// active task worktrees. Those are protected by default because removing
    /// an install tree leaves a live checkout unusable until it is rehydrated.
    #[arg(long)]
    pub include_active_worktrees: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum CleanupArtifactsSortArg {
    #[default]
    Discovery,
    Size,
}

#[derive(Args, Debug, PartialEq, Eq)]
pub struct CleanupWorktreesArgs {
    /// Cleanup a specific configured provider. Repeatable.
    #[arg(long = "provider", value_name = "ID", conflicts_with = "all_providers")]
    pub provider: Vec<String>,
    /// Cleanup every enabled configured provider.
    #[arg(long)]
    pub all_providers: bool,
    /// Apply cleanup. Omit for provider preview/dry-run output.
    #[arg(long)]
    pub apply: bool,
}

/// Naming for one cleanup category plus its specialist command.
#[derive(Clone, Copy)]
pub struct CleanupInventoryCategoryMetadata {
    pub category: &'static str,
    pub include_arg: &'static str,
    pub dry_run_command: &'static str,
    pub apply_command: &'static str,
}

impl CleanupInventoryCategoryMetadata {
    pub fn specialist_command(self, apply: bool) -> &'static str {
        if apply {
            self.apply_command
        } else {
            self.dry_run_command
        }
    }

    pub fn canonical_cleanup_command(self, apply: bool) -> String {
        let command = format!("homeboy cleanup --include {}", self.include_arg);
        if apply {
            format!("{command} --apply")
        } else {
            command
        }
    }
}

pub const RUNNER_DOWNLOADS_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "runner_downloads",
        include_arg: "runner-downloads",
        dry_run_command: "homeboy runs artifact cleanup-downloads",
        apply_command: "homeboy runs artifact cleanup-downloads --apply",
    };

/// Renders the aggregate cleanup command for the managed runtime-temp override.
pub fn runtime_tmp_commands(apply: bool, managed_older_than_days: u64) -> (String, String) {
    let apply_arg = if apply { " --apply" } else { "" };
    let command = format!(
        "homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days {managed_older_than_days}{apply_arg}"
    );
    (command.clone(), command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[derive(Parser)]
    struct CleanupParserTest {
        #[command(flatten)]
        cleanup: CleanupArgs,
    }

    #[test]
    fn parser_accepts_cleanup_front_door_and_managed_runtime_override() {
        let parsed = CleanupParserTest::parse_from([
            "cleanup",
            "--include",
            "repo-artifacts,task-worktrees",
            "--exclude",
            "runtime-tmp",
            "--apply",
        ]);
        assert!(parsed.cleanup.apply);
        assert_eq!(
            parsed.cleanup.include,
            vec![
                CleanupCategoryArg::RepoArtifacts,
                CleanupCategoryArg::TaskWorktrees
            ]
        );
        assert_eq!(parsed.cleanup.exclude, vec![CleanupCategoryArg::RuntimeTmp]);
    }

    #[test]
    fn parser_accepts_cleanup_subcommands_and_managed_runtime_override() {
        let parsed = CleanupParserTest::parse_from([
            "cleanup",
            "--include",
            "runtime-tmp",
            "--runtime-tmp-managed-older-than-days",
            "0",
        ]);
        assert_eq!(parsed.cleanup.include, vec![CleanupCategoryArg::RuntimeTmp]);
        assert_eq!(parsed.cleanup.runtime_tmp_managed_older_than_days, Some(0));

        let parsed = CleanupParserTest::parse_from([
            "cleanup",
            "artifacts",
            "--sort",
            "size",
            "--limit",
            "7",
            "--merged-only",
        ]);
        let Some(CleanupCommand::Artifacts(args)) = parsed.cleanup.command else {
            panic!("expected cleanup artifacts command");
        };
        assert_eq!(args.sort, CleanupArtifactsSortArg::Size);
        assert_eq!(args.limit, Some(7));
        assert!(args.merged_only);

        let parsed = CleanupParserTest::parse_from([
            "cleanup",
            "retained-storage",
            "--limit",
            "3",
            "--cursor",
            "prior-reference",
        ]);
        let Some(CleanupCommand::RetainedStorage(args)) = parsed.cleanup.command else {
            panic!("expected retained storage command");
        };
        assert_eq!(args.limit, 3);
        assert_eq!(args.cursor.as_deref(), Some("prior-reference"));
    }

    #[test]
    fn cleanup_reference_surface_and_canonical_commands_are_stable() {
        let command = CleanupParserTest::command();
        assert!(command
            .get_arguments()
            .any(|arg| arg.get_long() == Some("include")));
        assert_eq!(
            RUNNER_DOWNLOADS_METADATA.canonical_cleanup_command(true),
            "homeboy cleanup --include runner-downloads --apply"
        );
        assert_eq!(
            runtime_tmp_commands(false, 0).0,
            "homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 0"
        );
    }
}
