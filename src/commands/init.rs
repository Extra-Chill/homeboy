use std::collections::{HashMap, HashSet};

use clap::Args;
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::context::{
    self, build_component_info, path_is_parent_of, ComponentGap, ContextOutput,
};
use homeboy::deploy;
use homeboy::extension::{
    extension_ready_status, is_extension_compatible, is_extension_linked, load_all_extensions,
};
use homeboy::{is_zero, is_zero_u32};
use homeboy::project::{self, Project};
use homeboy::server::{self, Server};
use homeboy::{changelog, git, version};
use std::path::{Path, PathBuf};

fn collect_focused_components(
    show_all: bool,
    relevant_ids: &HashSet<String>,
    all_components: Vec<Component>,
    all_projects: &[Project],
) -> Vec<Component> {
    if show_all {
        return all_components;
    }

    let mut by_id: HashMap<String, Component> = all_components
        .into_iter()
        .filter(|c| relevant_ids.contains(&c.id))
        .map(|component| (component.id.clone(), component))
        .collect();

    for project in all_projects {
        for attachment in &project.components {
            if !relevant_ids.contains(&attachment.id) || by_id.contains_key(&attachment.id) {
                continue;
            }

            if let Some(mut component) =
                component::discover_from_portable(Path::new(&attachment.local_path))
            {
                component.id = attachment.id.clone();
                by_id.insert(component.id.clone(), component);
            }
        }
    }

    by_id.into_values().collect()
}

use super::utils::args::HiddenJsonArgs;
use super::CmdResult;

#[derive(Args)]
pub struct InitArgs {
    /// Show all components, extensions, projects, and servers
    #[arg(long, short = 'a')]
    pub all: bool,

    #[command(flatten)]
    pub json_args: HiddenJsonArgs,
}

#[derive(Debug, Serialize)]
pub struct GapSummary {
    pub component_id: String,
    pub field: String,
    pub reason: String,
    pub command: String,
}

#[derive(Debug, Serialize)]
pub struct InitStatus {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ready_to_deploy: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs_version_bump: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub docs_only: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub has_uncommitted: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub config_gaps: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gap_details: Vec<GapSummary>,
}

#[derive(Debug, Serialize)]
pub struct InitSummary {
    pub total_components: usize,
    pub by_extension: HashMap<String, usize>,
    pub by_status: HashMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct ComponentSummary {
    pub id: String,
    pub path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    pub status: String,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub commits_since_version: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub code_commits: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub docs_only_commits: u32,
}

#[derive(Debug, Serialize)]
pub struct InitOutput {
    pub command: &'static str,
    pub status: InitStatus,
    pub summary: InitSummary,
    pub context: ContextOutput,
    pub next_steps: Vec<String>,
    pub components: Vec<ComponentSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<Server>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<ProjectListItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<ExtensionEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_release: Option<ReleaseSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<ChangelogSnapshot>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub agent_context_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectListItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sub_targets: Vec<String>,
}

impl From<Project> for ProjectListItem {
    fn from(p: Project) -> Self {
        Self {
            id: p.id.clone(),
            domain: p.domain,
            sub_targets: p
                .sub_targets
                .iter()
                .filter_map(|st| project::slugify_id(&st.name).ok())
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ExtensionEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: String,
    pub compatible: bool,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    pub linked: bool,
}

#[derive(Debug, Serialize)]
pub struct VersionSnapshot {
    pub component_id: String,
    pub version: String,
    pub targets: Vec<version::VersionTargetInfo>,
}

#[derive(Debug, Serialize)]
pub struct GitSnapshot {
    pub branch: String,
    pub clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_since_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_warning: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReleaseSnapshot {
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChangelogSnapshot {
    pub path: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<String>>,
}

pub type ComponentReleaseState = homeboy::deploy::ReleaseState;

#[derive(Debug, Clone, Serialize)]
pub struct ComponentWithState {
    #[serde(flatten)]
    pub component: Component,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_state: Option<ComponentReleaseState>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<ComponentGap>,
}

pub fn run(args: InitArgs, _global: &super::GlobalArgs) -> CmdResult<InitOutput> {
    // Get context for current directory
    let (context_output, _) = context::run(None)?;

    // Collect relevant component IDs from context
    let relevant_ids: HashSet<String> = context_output
        .matched_components
        .iter()
        .chain(context_output.contained_components.iter())
        .cloned()
        .collect();

    // Load all data sources
    let all_components = component::inventory().unwrap_or_default();
    let all_projects = project::list().unwrap_or_default();
    let all_servers = server::list().unwrap_or_default();
    let all_extensions = load_all_extensions().unwrap_or_default();

    // Determine if we should show focused output
    let show_all = args.all || relevant_ids.is_empty();

    // Filter components and calculate release state
    let filtered_components =
        collect_focused_components(show_all, &relevant_ids, all_components, &all_projects);

    // Wrap components with release state and gaps
    let cwd = std::env::current_dir().ok();
    let components_with_state: Vec<ComponentWithState> = filtered_components
        .into_iter()
        .map(|component| {
            let release_state = deploy::calculate_release_state(&component);
            // Calculate gaps for contained components (parent context)
            let gaps = if let Some(ref cwd_path) = cwd {
                if path_is_parent_of(cwd_path, &component.local_path) {
                    build_component_info(&component).gaps
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            ComponentWithState {
                component,
                release_state,
                gaps,
            }
        })
        .collect();

    // Compute status buckets
    let release_buckets = deploy::bucket_release_states(
        components_with_state
            .iter()
            .map(|comp| (comp.component.id.as_str(), comp.release_state.as_ref())),
    );
    let status = compute_status(&components_with_state, &release_buckets);

    // Compute summary
    let summary = compute_summary(&components_with_state);

    // Get extension IDs linked to matched components
    let linked_extension_ids: HashSet<String> = components_with_state
        .iter()
        .filter_map(|c| c.component.extensions.as_ref())
        .flat_map(|m| m.keys().cloned())
        .collect();

    // Filter extensions: linked extensions + platform extensions (runtime.is_none())
    let extensions: Vec<ExtensionEntry> = all_extensions
        .iter()
        .filter(|m| show_all || linked_extension_ids.contains(&m.id) || m.executable.is_none())
        .map(|m| {
            let ready_status = extension_ready_status(m);
            ExtensionEntry {
                id: m.id.clone(),
                name: m.name.clone(),
                version: m.version.clone(),
                description: m
                    .description
                    .as_ref()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("")
                    .to_string(),
                runtime: if m.executable.is_some() {
                    "executable"
                } else {
                    "platform"
                }
                .to_string(),
                compatible: is_extension_compatible(m, None),
                ready: ready_status.ready,
                ready_reason: ready_status.reason,
                ready_detail: ready_status.detail,
                linked: is_extension_linked(&m.id),
            }
        })
        .collect();

    // Filter projects: those containing relevant components
    let filtered_projects: Vec<Project> = if show_all {
        all_projects
    } else {
        all_projects
            .into_iter()
            .filter(|p| p.components.iter().any(|c| relevant_ids.contains(&c.id)))
            .collect()
    };

    // Get server IDs from filtered projects
    let relevant_server_ids: HashSet<String> = filtered_projects
        .iter()
        .filter_map(|p| p.server_id.clone())
        .collect();

    // Convert projects to list items
    let projects: Vec<ProjectListItem> = filtered_projects
        .into_iter()
        .map(ProjectListItem::from)
        .collect();

    // Filter servers
    let servers: Vec<Server> = if show_all {
        all_servers
    } else {
        all_servers
            .into_iter()
            .filter(|s| relevant_server_ids.contains(&s.id))
            .collect()
    };

    // Build actionable next_steps based on status
    let next_steps = build_actionable_next_steps(
        &status,
        &context_output,
        &components_with_state,
        &projects,
        &linked_extension_ids,
        &all_extensions,
    );

    let version_snapshot = if context_output.managed {
        resolve_version_snapshot(&components_with_state)
    } else {
        None
    };
    let git_snapshot = resolve_git_snapshot(
        context_output.git_root.as_ref(),
        version_snapshot.as_ref().map(|v| v.version.as_str()),
    );
    let (last_release, changelog_snapshot) = resolve_changelog_snapshots(&components_with_state);

    let mut warnings = validate_version_targets(&components_with_state);
    if let Some(alignment_warning) =
        validate_version_baseline_alignment(&version_snapshot, &git_snapshot)
    {
        warnings.push(alignment_warning);
    }

    let agent_context_files = resolve_agent_context_files(context_output.git_root.as_ref());

    // Convert components to compact summary format
    let components = build_component_summaries(&components_with_state, cwd.as_ref());

    Ok((
        InitOutput {
            command: "init",
            status,
            summary,
            context: context_output,
            next_steps,
            components,
            servers,
            projects,
            extensions,
            version: version_snapshot,
            git: git_snapshot,
            last_release,
            changelog: changelog_snapshot,
            agent_context_files,
            warnings,
        },
        0,
    ))
}

fn compute_status(
    components: &[ComponentWithState],
    release_buckets: &homeboy::deploy::ReleaseStateBuckets,
) -> InitStatus {
    let mut config_gaps = 0;
    let mut gap_details = Vec::new();

    for comp in components {
        let id = &comp.component.id;

        // Collect gap details
        for gap in &comp.gaps {
            config_gaps += 1;
            gap_details.push(GapSummary {
                component_id: id.clone(),
                field: gap.field.clone(),
                reason: gap.reason.clone(),
                command: gap.command.clone(),
            });
        }

    }

    InitStatus {
        ready_to_deploy: release_buckets.ready_to_deploy.clone(),
        needs_version_bump: release_buckets.needs_bump.clone(),
        docs_only: release_buckets.docs_only.clone(),
        has_uncommitted: release_buckets.has_uncommitted.clone(),
        config_gaps,
        gap_details,
    }
}

fn compute_summary(components: &[ComponentWithState]) -> InitSummary {
    let mut by_extension: HashMap<String, usize> = HashMap::new();
    let mut by_status: HashMap<String, usize> = HashMap::new();

    for comp in components {
        // Count by extension
        if let Some(ref extensions) = comp.component.extensions {
            for extension_id in extensions.keys() {
                *by_extension.entry(extension_id.clone()).or_insert(0) += 1;
            }
        }

        // Count by status
        let status = deploy::classify_release_state(comp.release_state.as_ref())
            .as_str()
            .to_string();
        *by_status.entry(status).or_insert(0) += 1;
    }

    InitSummary {
        total_components: components.len(),
        by_extension,
        by_status,
    }
}

fn shorten_path(path: &str, cwd: Option<&PathBuf>) -> String {
    let path_buf = PathBuf::from(path);
    if let Some(cwd_path) = cwd {
        if let Ok(relative) = path_buf.strip_prefix(cwd_path) {
            let rel_str = relative.to_string_lossy().to_string();
            if !rel_str.is_empty() {
                return rel_str;
            }
            return ".".to_string();
        }
    }
    // Try to shorten to home-relative path
    if let Ok(home_str) = std::env::var("HOME") {
        let home = PathBuf::from(&home_str);
        if let Ok(relative) = path_buf.strip_prefix(&home) {
            return format!("~/{}", relative.to_string_lossy());
        }
    }
    path.to_string()
}

fn build_component_summaries(
    components: &[ComponentWithState],
    cwd: Option<&PathBuf>,
) -> Vec<ComponentSummary> {
    components
        .iter()
        .map(|comp| {
            let status = deploy::classify_release_state(comp.release_state.as_ref())
                .as_str()
                .to_string();
            let (commits, code, docs) = comp
                .release_state
                .as_ref()
                .map(|s| (s.commits_since_version, s.code_commits, s.docs_only_commits))
                .unwrap_or((0, 0, 0));

            let mut extensions = comp
                .component
                .extensions
                .as_ref()
                .map(|m| m.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            extensions.sort();

            ComponentSummary {
                id: comp.component.id.clone(),
                path: shorten_path(&comp.component.local_path, cwd),
                extensions,
                status,
                commits_since_version: commits,
                code_commits: code,
                docs_only_commits: docs,
            }
        })
        .collect()
}

fn build_actionable_next_steps(
    status: &InitStatus,
    context_output: &ContextOutput,
    components: &[ComponentWithState],
    projects: &[ProjectListItem],
    linked_extension_ids: &HashSet<String>,
    all_extensions: &[homeboy::extension::ExtensionManifest],
) -> Vec<String> {
    let mut next_steps = Vec::new();

    // Priority 1: Uncommitted changes
    if !status.has_uncommitted.is_empty() {
        let count = status.has_uncommitted.len();
        if count == 1 {
            next_steps.push(format!(
                "1 component has uncommitted changes: `{}`",
                status.has_uncommitted[0]
            ));
        } else {
            next_steps.push(format!(
                "{} components have uncommitted changes. Review with `homeboy changes <id>`.",
                count
            ));
        }
    }

    // Priority 2: Needs version bump
    if !status.needs_version_bump.is_empty() {
        let count = status.needs_version_bump.len();
        if count == 1 {
            next_steps.push(format!(
                "1 component has unreleased commits: `{}`. Release with `homeboy release {}`.",
                status.needs_version_bump[0], status.needs_version_bump[0]
            ));
        } else {
            next_steps.push(format!(
                "{} components have unreleased commits. Release with `homeboy release <id>`.",
                count
            ));
        }
    }

    // Priority 2.5: Docs-only changes (informational, no action needed)
    if !status.docs_only.is_empty() {
        let count = status.docs_only.len();
        if count == 1 {
            next_steps.push(format!(
                "1 component has docs-only changes: `{}`. No version bump needed.",
                status.docs_only[0]
            ));
        } else {
            next_steps.push(format!(
                "{} components have docs-only changes. No version bump needed.",
                count
            ));
        }
    }

    // Priority 3: Ready to deploy
    if !status.ready_to_deploy.is_empty() && status.has_uncommitted.is_empty() {
        let count = status.ready_to_deploy.len();
        if count == 1 {
            next_steps.push(format!(
                "1 component ready to deploy: `{}`. Deploy with `homeboy deploy {}`.",
                status.ready_to_deploy[0], status.ready_to_deploy[0]
            ));
        } else {
            next_steps.push(format!(
                "{} components ready to deploy. Run `homeboy deploy <id>`.",
                count
            ));
        }
    }

    // Config gaps (informational)
    if status.config_gaps > 0 {
        next_steps.push(format!(
            "{} config gaps detected. Run `homeboy component show <id>` for details.",
            status.config_gaps
        ));
    }

    // Context-specific guidance
    if context_output.managed && !components.is_empty() {
        let comp_id = &components[0].component.id;
        next_steps.push(format!(
            "You're in {}. Common: `homeboy build`, `homeboy deploy`, `homeboy release`.",
            comp_id
        ));
    }

    // Documentation guidance
    if Path::new("CLAUDE.md").exists() || Path::new("AGENTS.md").exists() {
        next_steps.push(
            "Read CLAUDE.md for repo-specific guidance. Run `homeboy docs commands/commands-index` for all commands.".to_string(),
        );
    }

    // CLI tools from linked extensions
    let cli_extensions: Vec<_> = all_extensions
        .iter()
        .filter(|m| linked_extension_ids.contains(&m.id))
        .filter_map(|m| {
            m.cli
                .as_ref()
                .map(|c| (c.tool.clone(), c.display_name.clone()))
        })
        .collect();

    if !cli_extensions.is_empty() && !projects.is_empty() {
        let project_id = &projects[0].id;
        for (tool, display_name) in &cli_extensions {
            next_steps.push(format!(
                "Run remote {} commands: `homeboy {} {} <command>`.",
                display_name, tool, project_id
            ));
        }
    }

    // Add context suggestion if present
    if let Some(suggestion) = context_output.suggestion.as_ref() {
        next_steps.push(format!("Suggestion: {}", suggestion));
    }

    // Check for outdated extensions (git-cloned only, skips linked)
    let mut outdated_extensions = Vec::new();
    for extension in all_extensions {
        if let Some(update) = homeboy::extension::check_update_available(&extension.id) {
            outdated_extensions.push(update);
        }
    }
    if !outdated_extensions.is_empty() {
        for update in &outdated_extensions {
            next_steps.push(format!(
                "Extension '{}' is outdated (v{}, {} commit{} behind). Run: `homeboy extension update {}`",
                update.extension_id,
                update.installed_version,
                update.behind_count,
                if update.behind_count == 1 { "" } else { "s" },
                update.extension_id,
            ));
        }
    }

    // Fallback for empty repos
    if components.is_empty() && !context_output.managed {
        next_steps.push(
            "Create a project: `homeboy project create <name> <domain> --server <id> --extension <id>`.".to_string(),
        );
        next_steps.push(
            "Create a component: `homeboy component create <name> --local-path . --remote-path <path> --project <id>`.".to_string(),
        );
    }

    next_steps
}

fn resolve_version_snapshot(components: &[ComponentWithState]) -> Option<VersionSnapshot> {
    let wrapper = components.first()?;
    let component = &wrapper.component;
    let snapshot = version::read_component_snapshot(component).ok()?;
    Some(VersionSnapshot {
        component_id: snapshot.component_id,
        version: snapshot.version,
        targets: snapshot.targets,
    })
}

fn resolve_git_snapshot(
    git_root: Option<&String>,
    current_version: Option<&str>,
) -> Option<GitSnapshot> {
    let root = git_root?;
    let snapshot = git::build_repo_baseline_snapshot(root, current_version).ok()?;
    Some(GitSnapshot {
        branch: snapshot.branch,
        clean: snapshot.clean,
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        commits_since_version: snapshot.commits_since_version,
        baseline_ref: snapshot.baseline_ref,
        baseline_warning: snapshot.baseline_warning,
    })
}

fn resolve_changelog_snapshots(
    components: &[ComponentWithState],
) -> (Option<ReleaseSnapshot>, Option<ChangelogSnapshot>) {
    let wrapper = match components.first() {
        Some(c) => c,
        None => return (None, None),
    };
    let component = &wrapper.component;

    let (last_release, changelog_snapshot) = match changelog::read_component_snapshots(component) {
        Ok((last_release, changelog_snapshot)) => (last_release, changelog_snapshot),
        Err(_) => return (None, None),
    };

    (
        last_release.map(|snapshot| ReleaseSnapshot {
            tag: snapshot.tag,
            date: snapshot.date,
            summary: snapshot.summary,
        }),
        changelog_snapshot.map(|snapshot| ChangelogSnapshot {
            path: snapshot.path,
            label: snapshot.label,
            items: if snapshot.items.is_empty() {
                None
            } else {
                Some(snapshot.items)
            },
        }),
    )
}

fn resolve_agent_context_files(git_root: Option<&String>) -> Vec<String> {
    let root = match git_root {
        Some(r) => r,
        None => return Vec::new(),
    };

    let path = PathBuf::from(root);
    git::list_tracked_markdown_files(&path).unwrap_or_default()
}

fn validate_version_targets(components: &[ComponentWithState]) -> Vec<String> {
    components
        .iter()
        .flat_map(|wrapper| version::build_init_warnings(&wrapper.component))
        .collect()
}

fn validate_version_baseline_alignment(
    version: &Option<VersionSnapshot>,
    git: &Option<GitSnapshot>,
) -> Option<String> {
    let version_snapshot = version.as_ref().map(|snapshot| version::ComponentVersionSnapshot {
        component_id: snapshot.component_id.clone(),
        version: snapshot.version.clone(),
        targets: snapshot.targets.clone(),
    });

    version::validate_baseline_alignment(
        version_snapshot.as_ref(),
        git.as_ref().and_then(|snapshot| snapshot.baseline_ref.as_deref()),
    )
}
