use std::collections::HashSet;

use clap::Args;
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::context::{self, ContextOutput};
use homeboy::module::{
    is_module_compatible, is_module_linked, load_all_modules, module_ready_status,
};
use homeboy::project::{self, Project};
use homeboy::server::{self, Server};
use homeboy::{changelog, git, version};
use std::fs;
use std::path::PathBuf;

use super::CmdResult;

#[derive(Args)]
pub struct InitArgs {
    /// Show all components, modules, projects, and servers
    #[arg(long, short = 'a')]
    pub all: bool,
}

#[derive(Debug, Serialize)]
pub struct InitOutput {
    pub command: &'static str,
    pub context: ContextOutput,
    pub next_steps: Vec<String>,
    pub servers: Vec<Server>,
    pub projects: Vec<ProjectListItem>,
    pub components: Vec<ComponentWithState>,
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
    pub version_baseline: Option<String>,
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

#[derive(Debug, Serialize)]
pub struct ComponentReleaseState {
    pub commits_since_version: u32,
    pub has_uncommitted_changes: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ComponentWithState {
    #[serde(flatten)]
    pub component: Component,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_state: Option<ComponentReleaseState>,
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

    // Wrap components with release state
    let components: Vec<ComponentWithState> = filtered_components
        .into_iter()
        .map(|component| {
            let release_state = calculate_component_release_state(&component);
            ComponentWithState {
                component,
                release_state,
            }
        })
        .collect();

    // Get module IDs linked to matched components
    let linked_module_ids: HashSet<String> = components
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

    let mut next_steps = vec![
        "Read CLAUDE.md and README.md for repo-specific guidance.".to_string(),
        "Run `homeboy docs documentation/index` for Homeboy documentation. Documentation workflows are self-directed—explore before asking.".to_string(),
        "Run `homeboy docs commands/commands-index` to browse available commands.".to_string(),
    ];

    if context_output.managed {
        next_steps.push("Run `homeboy context` to inspect local config state.".to_string());
        if !components.is_empty() {
            next_steps
                .push("Run `homeboy component show <id>` to inspect a component.".to_string());
        }
    } else if !context_output.contained_components.is_empty() {
        next_steps.push("Run `homeboy component show <id>` for a contained component.".to_string());
    } else {
        next_steps.push(
            "Create a project with `homeboy project create <name> <domain> --server <server_id> --module <module_id>`.".to_string(),
        );
        next_steps.push(
            "Create a component with `homeboy component create <name> --local-path . --remote-path <path> --project <project_id>`.".to_string(),
        );
    }

    // If agent context file exists, add documentation guidance
    if std::path::Path::new("CLAUDE.md").exists() || std::path::Path::new("AGENTS.md").exists() {
        next_steps.push(
            "For documentation tasks: Run `homeboy changes <component-id>` first, then follow `homeboy docs documentation/alignment`. No clarification needed.".to_string(),
        );
    }

    if let Some(suggestion) = context_output.suggestion.as_ref() {
        next_steps.push(format!("Suggestion: {}", suggestion));
    }

    // Check for linked modules with CLI tools (e.g., WordPress module's `homeboy wp`)
    let cli_modules: Vec<_> = all_modules
        .iter()
        .filter(|m| linked_module_ids.contains(&m.id))
        .filter_map(|m| m.cli.as_ref().map(|c| (c.tool.clone(), c.display_name.clone())))
        .collect();

    if !cli_modules.is_empty() && !projects.is_empty() {
        let project_id = &projects[0].id;
        for (tool, display_name) in &cli_modules {
            next_steps.push(format!(
                "Run remote {} commands: `homeboy {} {} <command>`",
                display_name, tool, project_id
            ));
        }
    }

    let version_snapshot = resolve_version_snapshot(&components);
    let git_snapshot = resolve_git_snapshot(context_output.git_root.as_ref());
    let (last_release, changelog_snapshot) = resolve_changelog_snapshots(&components);
    let warnings = validate_version_targets(&components);
    let agent_context_files = resolve_agent_context_files(context_output.git_root.as_ref());

    Ok((
        InitOutput {
            command: "init",
            context: context_output,
            next_steps,
            servers,
            projects,
            components,
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

fn calculate_component_release_state(component: &Component) -> Option<ComponentReleaseState> {
    let path = &component.local_path;

    let baseline = git::detect_baseline_for_path(path).ok()?;

    let commits = git::get_commits_since_tag(path, baseline.reference.as_deref())
        .ok()
        .map(|c| c.len() as u32)
        .unwrap_or(0);

    let uncommitted = git::get_uncommitted_changes(path)
        .ok()
        .map(|u| u.has_changes)
        .unwrap_or(false);

    Some(ComponentReleaseState {
        commits_since_version: commits,
        has_uncommitted_changes: uncommitted,
        baseline_ref: baseline.reference,
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

fn resolve_git_snapshot(git_root: Option<&String>) -> Option<GitSnapshot> {
    let root = git_root?;
    let snapshot = git::get_repo_snapshot(root).ok()?;

    // Get release state info (commits since last version tag)
    let baseline = git::detect_baseline_for_path(root).ok();
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
        version_baseline: baseline.and_then(|b| b.reference),
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
    changelog_path: &PathBuf,
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
    let re = regex::Regex::new(r"\[?(\d+\.\d+\.\d+)\]?").ok()?;
    re.captures(label)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn parse_date_label(label: &str) -> Option<String> {
    let re = regex::Regex::new(r"\d{4}-\d{2}-\d{2}").ok()?;
    re.find(label).map(|m| m.as_str().to_string())
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
    }
    warnings
}
