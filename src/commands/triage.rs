use clap::{Args, Subcommand};
use homeboy::triage::{self, TriageOptions, TriageOutput, TriageTarget};

use super::CmdResult;

#[derive(Args)]
pub struct TriageArgs {
    #[command(subcommand)]
    command: TriageCommand,

    /// Include issues in the report. Defaults to issues + PRs when neither is set.
    #[arg(long, global = true)]
    issues: bool,

    /// Include pull requests in the report. Defaults to issues + PRs when neither is set.
    #[arg(long, global = true)]
    prs: bool,

    /// Show work assigned to or authored by the authenticated GitHub user.
    #[arg(long, global = true)]
    mine: bool,

    /// Restrict to issues/PRs assigned to this GitHub user.
    #[arg(long, global = true, value_name = "USER")]
    assigned: Option<String>,

    /// Restrict to items carrying this label. Repeatable.
    #[arg(long, global = true, value_name = "LABEL")]
    label: Vec<String>,

    /// Restrict PRs to review-required items.
    #[arg(long, global = true)]
    needs_review: bool,

    /// Restrict PRs to failing-check items.
    #[arg(long, global = true)]
    failing_checks: bool,

    /// Mark issues/PRs stale after this many days (`14` or `14d`).
    #[arg(long, global = true, value_name = "DAYS")]
    stale: Option<String>,

    /// Maximum items fetched per repo for each item type.
    #[arg(long, global = true, default_value_t = 30)]
    limit: usize,
}

#[derive(Subcommand)]
enum TriageCommand {
    /// Triage one registered component.
    Component { component_id: String },
    /// Triage every component attached to a project.
    Project { project_id: String },
    /// Triage unique components used across a fleet.
    Fleet { fleet_id: String },
    /// Triage components declared in a local rig spec.
    Rig { rig_id: String },
    /// Triage every configured project, rig, and registered component once per repo.
    Workspace,
}

pub fn run(args: TriageArgs, _global: &super::GlobalArgs) -> CmdResult<TriageOutput> {
    let include_issues = args.issues || !args.prs;
    let include_prs = args.prs || !args.issues;
    let options = TriageOptions {
        include_issues,
        include_prs,
        mine: args.mine,
        assigned: args.assigned,
        labels: args.label,
        needs_review: args.needs_review,
        failing_checks: args.failing_checks,
        stale_days: match args.stale {
            Some(value) => Some(triage::parse_stale_days(&value)?),
            None => None,
        },
        limit: args.limit,
    };

    let target = match args.command {
        TriageCommand::Component { component_id } => TriageTarget::Component(component_id),
        TriageCommand::Project { project_id } => TriageTarget::Project(project_id),
        TriageCommand::Fleet { fleet_id } => TriageTarget::Fleet(fleet_id),
        TriageCommand::Rig { rig_id } => TriageTarget::Rig(rig_id),
        TriageCommand::Workspace => TriageTarget::Workspace,
    };

    Ok((triage::run(target, options)?, 0))
}
