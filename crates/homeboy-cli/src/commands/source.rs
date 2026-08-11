use clap::{Args, Subcommand};
use serde::Serialize;
use std::path::PathBuf;

use super::CmdResult;

#[derive(Args)]
pub struct SourceArgs {
    #[command(subcommand)]
    command: SourceCommand,
}

#[derive(Subcommand)]
enum SourceCommand {
    /// Check whether a directory satisfies the sealed Lab source-package policy
    Package {
        #[command(subcommand)]
        command: SourcePackageCommand,
    },
}

#[derive(Subcommand)]
enum SourcePackageCommand {
    /// Scan a source directory without creating any Homeboy resources
    Check {
        /// Source directory to inspect
        #[arg(long, value_name = "ROOT")]
        path: PathBuf,
    },
}

#[derive(Debug, Serialize)]
pub struct SourceOutput {
    pub source_package: homeboy::runner::runner_staging_operation::SourcePackageCheckVerdict,
}

pub fn run(args: SourceArgs) -> CmdResult<SourceOutput> {
    let SourceCommand::Package {
        command: SourcePackageCommand::Check { path },
    } = args.command;
    let verdict = homeboy::runner::runner_staging_operation::scan_source_package(&path).verdict;
    let exit_code = if verdict.valid { 0 } else { 1 };
    Ok((
        SourceOutput {
            source_package: verdict,
        },
        exit_code,
    ))
}

#[cfg(test)]
mod tests {
    use super::{run, SourceArgs, SourceCommand, SourcePackageCommand};
    use std::path::PathBuf;

    #[test]
    fn check_returns_a_nonzero_verdict_without_mutating_the_source_tree() {
        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("source.txt"), "source").expect("write source");
        let before = std::fs::read_dir(source.path())
            .expect("read source")
            .count();

        let (output, exit_code) = run(SourceArgs {
            command: SourceCommand::Package {
                command: SourcePackageCommand::Check {
                    path: PathBuf::from(source.path()),
                },
            },
        })
        .expect("check");

        assert_eq!(exit_code, 0);
        assert!(output.source_package.valid);
        assert!(output.source_package.accepted.is_some());
        assert!(output.source_package.partial.is_none());
        assert!(output.source_package.blocked.is_empty());
        assert_eq!(
            before,
            std::fs::read_dir(source.path())
                .expect("read source")
                .count()
        );
    }
}
