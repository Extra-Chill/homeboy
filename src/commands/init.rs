use std::collections::{HashMap, HashSet};

use clap::Args;
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::context::{self, build_component_info, path_is_parent_of, ComponentGap, ContextOutput};
use homeboy::module::{
    is_module_compatible, is_module_linked, load_all_modules, module_ready_status,
};
use homeboy::project::{self, Project};
use homeboy::server::{self, Server};
use homeboy::utils::parser;
use homeboy::{changelog, git, version};
use std::fs;
use std::path::{Path, PathBuf};

use super::CmdResult;

#[derive(Args)]
pub struct InitArgs {
    /// Show all components, modules, projects, and servers
    #[arg(long, short = 'a')]
    pub all: bool,

    /// Accept --json for compatibility (output is JSON by default)
    #[arg(long, hide = true)]
    pub json: bool,
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

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Serialize)]
pub struct InitSummary {
    pub total_components: usize,
    pub by_module: HashMap<String, usize>,
    pub by_status: HashMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct ComponentSummary {
    pub id: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub commits_since_version: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub code_commits: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub docs_only_commits: u32,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
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
    pub modules: Vec<ModuleEntry>,
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
pub struct ModuleEntry {
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

#[derive(Debug, Clone, Serialize)]
pub struct ComponentReleaseState {
    pub commits_since_version: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub code_commits: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub docs_only_commits: u32,
    pub has_uncommitted_changes: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentWithState {
    #[serde(flatten)]
    pub component: Component,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_state: Option<ComponentReleaseState>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<ComponentGap>,
}

pub fn run_json(args: InitArgs) -> CmdResult<InitOutput> {
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
    let all_components = component::list().unwrap_or_default();
    let all_projects = project::list().unwrap_or_default();
    let all_servers = server::list().unwrap_or_default();
    let all_modules = load_all_modules().unwrap_or_default();

    // Determine if we should show focused output
    let show_all = args.all || relevant_ids.is_empty();

    // Filter components and calculate release state
    let filtered_components: Vec<Component> = if show_all {
        all_components
    } else {
        all_components
            .into_iter()
            .filter(|c| relevant_ids.contains(&c.id))
            .collect()
    };

    // Wrap components with release state and gaps
    let cwd = std::env::current_dir().ok();
    let components_with_state: Vec<ComponentWithState> = filtered_components
        .into_iter()
        .map(|component| {
            let release_state = calculate_component_release_state(&component);
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
    let status = compute_status(&components_with_state);

    // Compute summary
    let summary = compute_summary(&components_with_state);

    // Get module IDs linked to matched components
    let linked_module_ids: HashSet<String> = components_with_state
        .iter()
        .filter_map(|c| c.component.modules.as_ref())
        .flat_map(|m| m.keys().cloned())
        .collect();

    // Filter modules: linked modules + platform modules (runtime.is_none())
    let modules: Vec<ModuleEntry> = all_modules
        .iter()
        .filter(|m| show_all || linked_module_ids.contains(&m.id) || m.runtime.is_none())
        .map(|m| {
            let ready_status = module_ready_status(m);
            ModuleEntry {
                id: m.id.clone(),
                name: m.name.clone(),
                version: m.version.clone(),
                description: m
                    .description
                    .as_ref()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("")
                    .to_string(),
                runtime: if m.runtime.is_some() {
                    "executable"
                } else {
                    "platform"
                }
                .to_string(),
                compatible: is_module_compatible(m, None),
                ready: ready_status.ready,
                ready_reason: ready_status.reason,
                ready_detail: ready_status.detail,
                linked: is_module_linked(&m.id),
            }
        })
        .collect();

    // Filter projects: those containing relevant components
    let filtered_projects: Vec<Project> = if show_all {
        all_projects
    } else {
        all_projects
            .into_iter()
            .filter(|p| p.component_ids.iter().any(|id| relevant_ids.contains(id)))
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
        &linked_module_ids,
        &all_modules,
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
            modules,
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

fn compute_status(components: &[ComponentWithState]) -> InitStatus {
    let mut ready_to_deploy = Vec::new();
    let mut needs_version_bump = Vec::new();
    let mut docs_only = Vec::new();
    let mut has_uncommitted = Vec::new();
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

        if let Some(ref state) = comp.release_state {
            if state.has_uncommitted_changes {
                has_uncommitted.push(id.clone());
            } else if state.code_commits > 0 {
                needs_version_bump.push(id.clone());
            } else if state.docs_only_commits > 0 {
                docs_only.push(id.clone());
            } else {
                ready_to_deploy.push(id.clone());
            }
        }
    }

    InitStatus {
        ready_to_deploy,
        needs_version_bump,
        docs_only,
        has_uncommitted,
        config_gaps,
        gap_details,
    }
}

fn compute_summary(components: &[ComponentWithState]) -> InitSummary {
    let mut by_module: HashMap<String, usize> = HashMap::new();
    let mut by_status: HashMap<String, usize> = HashMap::new();

    for comp in components {
        // Count by module
        if let Some(ref modules) = comp.component.modules {
            for module_id in modules.keys() {
                *by_module.entry(module_id.clone()).or_insert(0) += 1;
            }
        }

        // Count by status
        let status = determine_component_status(comp);
        *by_status.entry(status).or_insert(0) += 1;
    }

    InitSummary {
        total_components: components.len(),
        by_module,
        by_status,
    }
}

fn determine_component_status(comp: &ComponentWithState) -> String {
    match &comp.release_state {
        Some(state) if state.has_uncommitted_changes => "uncommitted".to_string(),
        Some(state) if state.code_commits > 0 => "needs_bump".to_string(),
        Some(state) if state.docs_only_commits > 0 => "docs_only".to_string(),
        Some(_) => "clean".to_string(),
        None => "unknown".to_string(),
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
            let status = determine_component_status(comp);
            let (commits, code, docs) = comp
                .release_state
                .as_ref()
                .map(|s| (s.commits_since_version, s.code_commits, s.docs_only_commits))
                .unwrap_or((0, 0, 0));

            // Get primary module
            let module = comp
                .component
                .modules
                .as_ref()
                .and_then(|m| m.keys().next().cloned());

            ComponentSummary {
                id: comp.component.id.clone(),
                path: shorten_path(&comp.component.local_path, cwd),
                module,
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
    linked_module_ids: &HashSet<String>,
    all_modules: &[homeboy::module::ModuleManifest],
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
                "1 component has unreleased commits: `{}`. Bump with `homeboy version bump {}`.",
                status.needs_version_bump[0], status.needs_version_bump[0]
            ));
        } else {
            next_steps.push(format!(
                "{} components have unreleased commits. Bump with `homeboy version bump <id>`.",
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
            "You're in {}. Common: `homeboy build`, `homeboy deploy`, `homeboy version bump`.",
            comp_id
        ));
    }

    // Documentation guidance
    if Path::new("CLAUDE.md").exists() || Path::new("AGENTS.md").exists() {
        next_steps.push(
            "Read CLAUDE.md for repo-specific guidance. Run `homeboy docs commands/commands-index` for all commands.".to_string(),
        );
    }

    // CLI tools from linked modules
    let cli_modules: Vec<_> = all_modules
        .iter()
        .filter(|m| linked_module_ids.contains(&m.id))
        .filter_map(|m| {
            m.cli
                .as_ref()
                .map(|c| (c.tool.clone(), c.display_name.clone()))
        })
        .collect();

    if !cli_modules.is_empty() && !projects.is_empty() {
        let project_id = &projects[0].id;
        for (tool, display_name) in &cli_modules {
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

    // Check for outdated modules (git-cloned only, skips linked)
    let mut outdated_modules = Vec::new();
    for module in all_modules {
        if let Some(update) = homeboy::module::check_update_available(&module.id) {
            outdated_modules.push(update);
        }
    }
    if !outdated_modules.is_empty() {
        for update in &outdated_modules {
            next_steps.push(format!(
                "Module '{}' is outdated (v{}, {} commit{} behind). Run: `homeboy module update {}`",
                update.module_id,
                update.installed_version,
                update.behind_count,
                if update.behind_count == 1 { "" } else { "s" },
                update.module_id,
            ));
        }
    }

    // Fallback for empty repos
    if components.is_empty() && !context_output.managed {
        next_steps.push(
            "Create a project: `homeboy project create <name> <domain> --server <id> --module <id>`.".to_string(),
        );
        next_steps.push(
            "Create a component: `homeboy component create <name> --local-path . --remote-path <path> --project <id>`.".to_string(),
        );
    }

    next_steps
}

fn calculate_component_release_state(component: &Component) -> Option<ComponentReleaseState> {
    let path = &component.local_path;

    // Get current version for alignment checking
    let current_version = version::read_component_version(component)
        .ok()
        .map(|info| info.version);

    let baseline = git::detect_baseline_with_version(path, current_version.as_deref()).ok()?;

    let commits = git::get_commits_since_tag(path, baseline.reference.as_deref())
        .ok()
        .unwrap_or_default();

    // Categorize commits into code vs docs-only
    let counts = git::categorize_commits(path, &commits);

    let uncommitted = git::get_uncommitted_changes(path)
        .ok()
        .map(|u| u.has_changes)
        .unwrap_or(false);

    Some(ComponentReleaseState {
        commits_since_version: counts.total,
        code_commits: counts.code,
        docs_only_commits: counts.docs_only,
        has_uncommitted_changes: uncommitted,
        baseline_ref: baseline.reference,
        baseline_warning: baseline.warning,
    })
}

fn resolve_version_snapshot(components: &[ComponentWithState]) -> Option<VersionSnapshot> {
    let wrapper = components.first()?;
    let component = &wrapper.component;
    let info = version::read_component_version(component).ok()?;
    Some(VersionSnapshot {
        component_id: component.id.clone(),
        version: info.version,
        targets: info.targets,
    })
}

fn resolve_git_snapshot(
    git_root: Option<&String>,
    current_version: Option<&str>,
) -> Option<GitSnapshot> {
    let root = git_root?;
    let snapshot = git::get_repo_snapshot(root).ok()?;

    // Get release state info with version alignment checking
    let baseline = git::detect_baseline_with_version(root, current_version).ok();
    let commits_since = baseline.as_ref().and_then(|b| {
        git::get_commits_since_tag(root, b.reference.as_deref())
            .ok()
            .map(|c| c.len() as u32)
    });

    Some(GitSnapshot {
        branch: snapshot.branch,
        clean: snapshot.clean,
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        commits_since_version: commits_since,
        baseline_ref: baseline.as_ref().and_then(|b| b.reference.clone()),
        baseline_warning: baseline.and_then(|b| b.warning),
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

    let changelog_path = match changelog::resolve_changelog_path(component) {
        Ok(path) => path,
        Err(_) => return (None, None),
    };
    let content = match fs::read_to_string(&changelog_path) {
        Ok(content) => content,
        Err(_) => return (None, None),
    };
    let settings = changelog::resolve_effective_settings(Some(component));

    let changelog_snapshot = build_changelog_snapshot(&content, &changelog_path, &settings);
    let last_release = build_last_release_snapshot(&content);

    (last_release, changelog_snapshot)
}

fn build_changelog_snapshot(
    content: &str,
    changelog_path: &Path,
    settings: &changelog::EffectiveChangelogSettings,
) -> Option<ChangelogSnapshot> {
    let items = extract_section_items(content, &settings.next_section_aliases);
    Some(ChangelogSnapshot {
        path: changelog_path.to_string_lossy().to_string(),
        label: settings.next_section_label.clone(),
        items: if items.is_empty() { None } else { Some(items) },
    })
}

fn build_last_release_snapshot(content: &str) -> Option<ReleaseSnapshot> {
    let lines: Vec<&str> = content.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with("## ") {
            continue;
        }

        let label = trimmed.trim_start_matches("## ").trim();
        if label.eq_ignore_ascii_case("unreleased")
            || label.starts_with("[Unreleased")
            || label.starts_with("[unreleased")
        {
            continue;
        }

        let Some(tag) = parse_version_label(label) else {
            continue;
        };

        let date = parse_date_label(label);
        let summary = extract_first_bullet(&lines, index + 1);

        return Some(ReleaseSnapshot {
            tag: format!("v{}", tag),
            date,
            summary,
        });
    }

    None
}

fn extract_section_items(content: &str, aliases: &[String]) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    let start = find_section_start(&lines, aliases);
    let Some(start_index) = start else {
        return Vec::new();
    };
    let end_index = find_section_end(&lines, start_index);
    let mut items = Vec::new();

    for line in &lines[start_index + 1..end_index] {
        let trimmed = line.trim();
        if trimmed.starts_with('-') {
            items.push(trimmed.trim_start_matches('-').trim().to_string());
        }
    }

    items
}

fn find_section_start(lines: &[&str], aliases: &[String]) -> Option<usize> {
    lines.iter().position(|line| {
        let trimmed = line.trim();
        if !trimmed.starts_with("## ") {
            return false;
        }
        let label = trimmed.trim_start_matches("## ").trim();
        aliases.iter().any(|alias| {
            let alias_trim = alias.trim().trim_matches(['[', ']']);
            let label_trim = label.trim().trim_matches(['[', ']']);
            alias_trim == label_trim
        })
    })
}

fn find_section_end(lines: &[&str], start: usize) -> usize {
    let mut index = start + 1;
    while index < lines.len() {
        if lines[index].trim().starts_with("## ") {
            break;
        }
        index += 1;
    }
    index
}

fn extract_first_bullet(lines: &[&str], start: usize) -> Option<String> {
    for line in &lines[start..] {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            break;
        }
        if trimmed.starts_with('-') {
            return Some(trimmed.trim_start_matches('-').trim().to_string());
        }
    }
    None
}

fn parse_version_label(label: &str) -> Option<String> {
    parser::extract_first(label, r"\[?(\d+\.\d+\.\d+)\]?")
}

fn parse_date_label(label: &str) -> Option<String> {
    parser::extract_first(label, r"(\d{4}-\d{2}-\d{2})")
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
    let mut warnings = Vec::new();
    for wrapper in components {
        let comp = &wrapper.component;

        // Check for missing patterns on configured targets
        if let Some(targets) = &comp.version_targets {
            for target in targets {
                if target.pattern.is_none()
                    && version::default_pattern_for_file(&target.file).is_none()
                {
                    warnings.push(format!(
                        "Component '{}' has version target '{}' with no pattern and no module default. Run: homeboy component set {} --version-targets @file.json",
                        comp.id, target.file, comp.id
                    ));
                }
            }
        }

        // Check for unconfigured patterns (version patterns found but not managed)
        let unconfigured = version::detect_unconfigured_patterns(comp);
        for pattern in &unconfigured {
            warnings.push(format!(
                "Unconfigured version pattern in '{}': {} found in {} (v{}). Add with: homeboy component add-version-target {} '{}' '{}'",
                comp.id, pattern.description, pattern.file, pattern.found_version,
                comp.id, pattern.file, pattern.pattern
            ));
        }
    }
    warnings
}

fn validate_version_baseline_alignment(
    version: &Option<VersionSnapshot>,
    git: &Option<GitSnapshot>,
) -> Option<String> {
    let version_snapshot = version.as_ref()?;
    let git_snapshot = git.as_ref()?;
    let baseline = git_snapshot.baseline_ref.as_ref()?;

    // Extract version from tag (v0.5.1 -> 0.5.1)
    let baseline_version = baseline.strip_prefix('v').unwrap_or(baseline);

    if version_snapshot.version != baseline_version {
        Some(format!(
            "Version mismatch: source files show {} but git baseline is {}. \
            Consider creating a tag or bumping the version.",
            version_snapshot.version, baseline
        ))
    } else {
        None
    }
}
