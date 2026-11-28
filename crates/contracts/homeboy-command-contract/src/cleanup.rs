//! Cleanup command parsing and canonical operator-command rendering.
//!
//! This is a command-surface contract, not a cleanup implementation. Keeping it
//! here lets parser and reference tests compile without execution adapters.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(Args, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupArgs {
    /// Apply cleanup across the selected categories. Omit for inventory dry-run output.
    /// Use the global `--placement local` to execute synchronously on this controller;
    /// the default submits a durable asynchronous controller job.
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

    /// Also reap `runner-downloads` cache directories that carry no download
    /// intent marker. Untagged caches are treated as operator-owned by default,
    /// so every one written before intent tagging existed is otherwise retained
    /// forever. The fixed age floor, the running-run veto, and the never-reap
    /// rule for operator pulls all still apply.
    #[arg(long)]
    pub include_untagged: bool,

    /// Override the configured terminal-run retention window for this invocation.
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<i64>,

    /// Override the age floor for metadata-backed runtime temp entries only.
    /// Unmanaged entries retain the configured runtime temp age floor.
    #[arg(long, value_name = "DAYS")]
    pub runtime_tmp_managed_older_than_days: Option<u64>,

    /// Override the per-pass scan limit and cap each default detail collection.
    #[arg(long, value_name = "N")]
    pub limit: Option<i64>,

    /// Include every candidate and retained-resource detail in the inspected page.
    /// Default output keeps representative detail within the shared response budget.
    #[arg(long, visible_alias = "verbose")]
    pub full: bool,

    /// Continue a bounded shared-store or controller-scratch inventory from its cursor.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,

    #[command(subcommand)]
    #[serde(skip)]
    pub command: Option<CleanupCommand>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, Serialize, Deserialize)]
#[value(rename_all = "kebab-case")]
pub enum CleanupCategoryArg {
    RepoArtifacts,
    TaskWorktrees,
    TerminalRuns,
    PersistedRunArtifacts,
    OrphanedArtifactBytes,
    RunnerDownloads,
    RunnerBinaryCaches,
    RemoteLabWorkspaces,
    RuntimeTmp,
    ControllerScratch,
    SharedCargoTargets,
    /// Isolated test homes abandoned by killed test processes. Proven by a
    /// filename marker plus the liveness of the PID stamped into it, so it can
    /// plan and apply with the observation store shut (#11073).
    LeakedTestHomes,
    /// Superseded copies in the durable release artifact store. These are
    /// referenced by durable release records, so `orphaned-artifact-bytes`
    /// correctly finds zero candidates against them and they need their own
    /// per-repository count and byte bounds (#14223).
    ReleaseArtifacts,
    /// Installed runtime providers own discovery and native reclaim for roots
    /// outside Homeboy worktrees.
    ExternalStorage,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum CleanupCommand {
    /// Inspect or remove declared reconstructable artifacts across repo worktrees
    Artifacts(CleanupArtifactsArgs),
    /// Explain retained Homeboy storage without deleting or reconciling resources.
    ///
    /// Reports lifecycle aggregates alongside root filesystem accounting, top-level
    /// stores, largest child paths, ownership classification, and cleanup guidance.
    RetainedStorage(CleanupRetainedStorageArgs),
    /// Run one configured, bounded retention pass
    AutomaticRetention,
    /// Read compact durable progress for an asynchronous cleanup apply
    Status(CleanupJobArgs),
    /// Re-submit a queued durable cleanup job after an interrupted client
    Resume(CleanupJobArgs),
}

#[derive(Args, Debug, PartialEq, Eq, Clone)]
pub struct CleanupJobArgs {
    /// Durable controller job identifier printed by cleanup --apply.
    pub job_id: String,
    /// Include the daemon's durable job record, including completed cleanup evidence.
    #[arg(long)]
    pub full: bool,
}

#[derive(Args, Debug, PartialEq, Eq)]
pub struct CleanupRetainedStorageArgs {
    /// Maximum rows per detail collection. Totals always cover all inspected sources.
    #[arg(long, default_value_t = 20, value_parser = parse_positive_usize)]
    pub limit: usize,
    /// Continue largest-byte examples after this deterministic reference token.
    #[arg(long, conflicts_with = "verbose")]
    pub cursor: Option<String>,
    /// Show all inspected lifecycle rows, aggregate keys, and filesystem detail.
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Args, Debug, PartialEq, Eq)]
pub struct CleanupArtifactsArgs {
    /// Apply cleanup. Omit for dry-run output.
    #[arg(long)]
    pub apply: bool,
    /// Clean artifacts from the Homeboy source checkout that built this binary.
    #[arg(long = "self", conflicts_with = "path")]
    pub self_artifacts: bool,
    /// Clean only this checkout instead of the current directory.
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Discover artifacts across every Git worktree in the selected repository.
    #[arg(long)]
    pub all_worktrees: bool,
    /// Also scan this temp root for detached Homeboy build artifacts. Repeatable.
    #[arg(long, value_name = "PATH")]
    pub temp_root: Vec<PathBuf>,
    /// Sort artifact candidates before reporting or applying cleanup.
    #[arg(long, value_enum, default_value = "discovery")]
    pub sort: CleanupArtifactsSortArg,
    /// Limit artifact candidates reported or removed, and cap each default detail collection.
    /// A positive limit ensures a continuation always makes progress.
    #[arg(
        long,
        value_name = "N",
        value_parser = parse_positive_usize
    )]
    pub limit: Option<usize>,
    /// Include all detail from the inspected artifact page.
    #[arg(long, visible_alias = "full")]
    pub verbose: bool,
    /// Resume repository-worktree artifact inventory at the cursor returned by
    /// an earlier bounded pass.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
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

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let limit = value.parse::<usize>().map_err(|error| error.to_string())?;
    if limit == 0 {
        Err("must be at least 1".to_string())
    } else {
        Ok(limit)
    }
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

/// Abandoned isolated test homes. There is no specialist command: the reclaim
/// predicate is one filename marker plus one liveness probe, and a second
/// spelling of that would be a second place for it to drift.
pub const LEAKED_TEST_HOMES_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "leaked_test_homes",
        include_arg: "leaked-test-homes",
        dry_run_command: "homeboy cleanup --include leaked-test-homes",
        apply_command: "homeboy cleanup --include leaked-test-homes --apply",
    };

/// Superseded durable release artifact copies. There is no specialist command:
/// the retention decision is per-repository rank plus two budgets, and a second
/// spelling of that would be a second place for it to drift.
pub const RELEASE_ARTIFACTS_METADATA: CleanupInventoryCategoryMetadata =
    CleanupInventoryCategoryMetadata {
        category: "release_artifacts",
        include_arg: "release-artifacts",
        dry_run_command: "homeboy cleanup --include release-artifacts",
        apply_command: "homeboy cleanup --include release-artifacts --apply",
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
    use clap::Parser;

    #[derive(Parser)]
    struct CleanupParserTest {
        #[command(flatten)]
        cleanup: CleanupArgs,
    }

    #[test]
    fn artifact_cleanup_rejects_zero_limit_before_emitting_a_continuation() {
        let error = match CleanupParserTest::try_parse_from([
            "cleanup",
            "artifacts",
            "--limit",
            "0",
            "--apply",
        ]) {
            Ok(_) => panic!("zero would produce a non-progressing continuation"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(error.to_string().contains("must be at least 1"));
    }

    /// Rendered operator commands must parse back through the real cleanup
    /// parser to the category they name, so guidance never drifts from flags.
    #[test]
    fn rendered_cleanup_commands_parse_back_to_their_category() {
        let cases = [
            (
                RUNNER_DOWNLOADS_METADATA.canonical_cleanup_command(true),
                CleanupCategoryArg::RunnerDownloads,
                true,
            ),
            (
                LEAKED_TEST_HOMES_METADATA.canonical_cleanup_command(false),
                CleanupCategoryArg::LeakedTestHomes,
                false,
            ),
            (
                RELEASE_ARTIFACTS_METADATA.canonical_cleanup_command(true),
                CleanupCategoryArg::ReleaseArtifacts,
                true,
            ),
            (
                runtime_tmp_commands(false, 0).0,
                CleanupCategoryArg::RuntimeTmp,
                false,
            ),
        ];

        for (command, category, apply) in cases {
            let argv = command
                .strip_prefix("homeboy ")
                .expect("rendered command starts with homeboy")
                .split_whitespace();
            let parsed = CleanupParserTest::try_parse_from(argv)
                .unwrap_or_else(|error| panic!("{command} must parse: {error}"));
            assert_eq!(parsed.cleanup.include, vec![category], "{command}");
            assert_eq!(parsed.cleanup.apply, apply, "{command}");
        }
        let runtime = runtime_tmp_commands(true, 7).0;
        let parsed = CleanupParserTest::parse_from(runtime.split_whitespace().skip(1));
        assert!(parsed.cleanup.apply);
        assert_eq!(parsed.cleanup.runtime_tmp_managed_older_than_days, Some(7));
    }
}
