use clap::Args;
use serde::Serialize;

use homeboy::component;
use homeboy::context;
use homeboy::git;
use homeboy::version;

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
        match classify_component(comp) {
            ComponentStatus::Uncommitted => uncommitted.push(comp.id.clone()),
            ComponentStatus::NeedsBump => needs_bump.push(comp.id.clone()),
            ComponentStatus::DocsOnly => docs_only.push(comp.id.clone()),
            ComponentStatus::Clean => ready_to_deploy.push(comp.id.clone()),
            ComponentStatus::Unknown => clean += 1,
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

enum ComponentStatus {
    Uncommitted,
    NeedsBump,
    DocsOnly,
    Clean,
    Unknown,
}

fn classify_component(component: &component::Component) -> ComponentStatus {
    let path = &component.local_path;

    let current_version = version::read_component_version(component)
        .ok()
        .map(|info| info.version);

    let baseline = match git::detect_baseline_with_version(path, current_version.as_deref()) {
        Ok(b) => b,
        Err(_) => return ComponentStatus::Unknown,
    };

    let commits = git::get_commits_since_tag(path, baseline.reference.as_deref())
        .ok()
        .unwrap_or_default();

    let counts = git::categorize_commits(path, &commits);

    let uncommitted = git::get_uncommitted_changes(path)
        .ok()
        .map(|u| u.has_changes)
        .unwrap_or(false);

    if uncommitted {
        ComponentStatus::Uncommitted
    } else if counts.code > 0 {
        ComponentStatus::NeedsBump
    } else if counts.docs_only > 0 {
        ComponentStatus::DocsOnly
    } else {
        ComponentStatus::Clean
    }
}
