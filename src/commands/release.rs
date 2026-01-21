use clap::{Args, ValueEnum};
use serde::Serialize;

use homeboy::release::{self, ReleasePlan, ReleaseRun};

use super::CmdResult;

#[derive(Clone, ValueEnum)]
pub enum BumpType {
    Patch,
    Minor,
    Major,
}

impl BumpType {
    pub fn as_str(&self) -> &'static str {
        match self {
            BumpType::Patch => "patch",
            BumpType::Minor => "minor",
            BumpType::Major => "major",
        }
    }
}

#[derive(Args)]
pub struct ReleaseArgs {
    /// Component ID
    #[arg(value_name = "COMPONENT")]
    component_id: String,

    /// Version bump type (patch, minor, major)
    #[arg(value_name = "BUMP_TYPE")]
    bump_type: BumpType,

    /// Preview what will happen without making changes
    #[arg(long)]
    dry_run: bool,

    /// Local-only release: bump + commit + tag (no push/publish even if configured)
    #[arg(long, conflicts_with = "publish")]
    local: bool,

    /// Force full pipeline: push + publish even if not configured as default
    #[arg(long, conflicts_with = "local")]
    publish: bool,

    /// Skip creating git tag
    #[arg(long)]
    no_tag: bool,

    /// Skip pushing to remote (implies no publish)
    #[arg(long)]
    no_push: bool,

    /// Accept --json for compatibility (output is JSON by default)
    #[arg(long, hide = true)]
    json: bool,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release")]
pub struct ReleaseOutput {
    pub result: ReleaseResult,
}

#[derive(Serialize)]
pub struct ReleaseResult {
    pub component_id: String,
    pub bump_type: String,
    pub dry_run: bool,
    pub local: bool,
    pub publish: bool,
    pub no_tag: bool,
    pub no_push: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<ReleasePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<ReleaseRun>,
}

pub fn run(args: ReleaseArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ReleaseOutput> {
    let options = release::ReleaseOptions {
        bump_type: args.bump_type.as_str().to_string(),
        dry_run: args.dry_run,
        local: args.local,
        publish: args.publish,
        no_tag: args.no_tag,
        no_push: args.no_push,
    };

    if args.dry_run {
        let plan = release::plan_unified(&args.component_id, &options)?;
        Ok((
            ReleaseOutput {
                result: ReleaseResult {
                    component_id: args.component_id,
                    bump_type: options.bump_type,
                    dry_run: true,
                    local: args.local,
                    publish: args.publish,
                    no_tag: args.no_tag,
                    no_push: args.no_push,
                    plan: Some(plan),
                    run: None,
                },
            },
            0,
        ))
    } else {
        let run_result = release::run_unified(&args.component_id, &options)?;
        Ok((
            ReleaseOutput {
                result: ReleaseResult {
                    component_id: args.component_id,
                    bump_type: options.bump_type,
                    dry_run: false,
                    local: args.local,
                    publish: args.publish,
                    no_tag: args.no_tag,
                    no_push: args.no_push,
                    plan: None,
                    run: Some(run_result),
                },
            },
            0,
        ))
    }
}
