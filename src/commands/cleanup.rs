use std::path::PathBuf;

use clap::{Args, Subcommand};
use homeboy::core::cleanup::{self, ArtifactCleanupOptions, ArtifactCleanupOutput};

use super::CmdResult;

#[derive(Args)]
pub struct CleanupArgs {
    #[command(subcommand)]
    pub command: CleanupCommand,
}

#[derive(Subcommand)]
pub enum CleanupCommand {
    /// Inspect or remove declared reconstructable artifacts across repo worktrees
    Artifacts(CleanupArtifactsArgs),
}

#[derive(Args)]
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

    /// Only reclaim artifacts from worktrees whose branch is already merged
    /// into its upstream. Preserves in-progress cooks' build dirs.
    #[arg(long)]
    pub merged_only: bool,
}

pub fn run(args: CleanupArgs, _global: &super::GlobalArgs) -> CmdResult<ArtifactCleanupOutput> {
    match args.command {
        CleanupCommand::Artifacts(args) => cleanup::cleanup_artifacts(ArtifactCleanupOptions {
            path: args.path,
            apply: args.apply,
            self_artifacts: args.self_artifacts,
            temp_roots: args.temp_root,
            merged_only: args.merged_only,
        })
        .map(|output| (output, 0)),
    }
}
