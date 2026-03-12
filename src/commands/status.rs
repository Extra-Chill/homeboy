use clap::Args;
use serde::Serialize;

use homeboy::component;
use homeboy::context;
use homeboy::deploy::{self, ReleaseStateStatus};

use super::CmdResult;

#[derive(Args)]
pub struct StatusArgs {
    /// Show only components with uncommitted changes
    #[arg(long)]
    pub uncommitted: bool,

    /// Show only components that need a version bump
    #[arg(long)]
    pub needs_bump: bool,

    /// Show only components ready to deploy
    #[arg(long)]
    pub ready: bool,

    /// Show only components with docs-only changes
    #[arg(long)]
    pub docs_only: bool,

    /// Show all components regardless of current directory context
    #[arg(long, short = 'a')]
    pub all: bool,
}

#[derive(Debug, Serialize)]
pub struct StatusOutput {
    pub command: &'static str,
    pub total: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncommitted: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs_bump: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ready_to_deploy: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub docs_only: Vec<String>,
    pub clean: usize,
}

pub fn run(args: StatusArgs, _global: &super::GlobalArgs) -> CmdResult<StatusOutput> {
    let (context_output, _) = context::run(None)?;

    let relevant_ids: std::collections::HashSet<String> = context_output
        .matched_components
        .iter()
        .chain(context_output.contained_components.iter())
        .cloned()
        .collect();

    let all_components = component::inventory().unwrap_or_default();

    let show_all = args.all || relevant_ids.is_empty();

    let components: Vec<component::Component> = if show_all {
        all_components
    } else {
        all_components
            .into_iter()
            .filter(|c| relevant_ids.contains(&c.id))
            .collect()
    };

    let total = components.len();

    let mut uncommitted = Vec::new();
    let mut needs_bump = Vec::new();
    let mut ready_to_deploy = Vec::new();
    let mut docs_only = Vec::new();
    let mut clean: usize = 0;

    for comp in &components {
        let status = deploy::calculate_release_state(comp)
            .map(|state| state.status())
            .unwrap_or(ReleaseStateStatus::Unknown);

        match status {
            ReleaseStateStatus::Uncommitted => uncommitted.push(comp.id.clone()),
            ReleaseStateStatus::NeedsBump => needs_bump.push(comp.id.clone()),
            ReleaseStateStatus::DocsOnly => docs_only.push(comp.id.clone()),
            ReleaseStateStatus::Clean => ready_to_deploy.push(comp.id.clone()),
            ReleaseStateStatus::Unknown => clean += 1,
        }
    }

    // Apply filters if any are set
    let has_filter = args.uncommitted || args.needs_bump || args.ready || args.docs_only;

    if has_filter {
        if !args.uncommitted {
            uncommitted.clear();
        }
        if !args.needs_bump {
            needs_bump.clear();
        }
        if !args.ready {
            ready_to_deploy.clear();
        }
        if !args.docs_only {
            docs_only.clear();
        }
    }

    Ok((
        StatusOutput {
            command: "status",
            total,
            uncommitted,
            needs_bump,
            ready_to_deploy,
            docs_only,
            clean,
        },
        0,
    ))
}
