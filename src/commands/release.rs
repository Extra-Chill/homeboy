use clap::Args;
use serde::Serialize;

use homeboy::release::{self, ReleaseCommandInput, ReleaseCommandResult};

use super::utils::args::{DryRunArgs, HiddenJsonArgs, PositionalComponentArgs};
use super::CmdResult;

#[derive(Args)]
pub struct ReleaseArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    #[command(flatten)]
    dry_run_args: DryRunArgs,

    #[command(flatten)]
    _json: HiddenJsonArgs,

    /// Deploy to all projects using this component after release
    #[arg(long)]
    deploy: bool,

    /// Recover from an interrupted release (tag + push current version)
    #[arg(long)]
    recover: bool,

    /// Skip pre-release lint and test checks
    #[arg(long)]
    skip_checks: bool,

    /// Allow a major version bump. Required when commits contain breaking changes.
    /// Without this flag, homeboy will warn and exit instead of releasing a major bump.
    #[arg(long)]
    major: bool,

    /// Skip publish/package steps (version bump + tag + push only).
    /// Use when CI handles publishing after the tag is pushed.
    #[arg(long)]
    skip_publish: bool,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release")]
pub struct ReleaseOutput {
    pub result: ReleaseCommandResult,
}

pub fn run(args: ReleaseArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ReleaseOutput> {
    let (result, exit_code) = release::run_command(ReleaseCommandInput {
        component_id: args.comp.id().to_string(),
        path_override: args.comp.path.clone(),
        dry_run: args.dry_run_args.dry_run,
        deploy: args.deploy,
        recover: args.recover,
        skip_checks: args.skip_checks,
        major: args.major,
        skip_publish: args.skip_publish,
    })?;

    Ok((ReleaseOutput { result }, exit_code))
}
