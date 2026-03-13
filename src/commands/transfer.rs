use clap::Args;

use homeboy::transfer::{self, TransferConfig};

use super::CmdResult;

// Re-export for dispatch compatibility
pub use homeboy::transfer::TransferOutput;

#[derive(Args)]
pub struct TransferArgs {
    /// Source: local path or server_id:/path
    pub source: String,

    /// Destination: local path or server_id:/path
    pub destination: String,

    /// Transfer directories recursively
    #[arg(short, long)]
    pub recursive: bool,

    /// Compress data during transfer
    #[arg(short, long)]
    pub compress: bool,

    /// Show what would be transferred without doing it
    #[arg(long)]
    pub dry_run: bool,

    /// Exclude patterns (can be specified multiple times)
    #[arg(long)]
    pub exclude: Vec<String>,
}

pub fn run(args: TransferArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<TransferOutput> {
    let config = TransferConfig {
        source: args.source,
        destination: args.destination,
        recursive: args.recursive,
        compress: args.compress,
        dry_run: args.dry_run,
        exclude: args.exclude,
    };

    transfer::transfer(&config)
}
