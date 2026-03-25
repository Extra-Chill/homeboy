//! build — extracted from report.rs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use crate::component::{self, Component};
use crate::deploy;
use crate::extension::{
    extension_ready_status, is_extension_compatible, is_extension_linked, load_all_extensions,
};
use crate::project::{self, Project};
use crate::server::{self, Server};
use crate::{changelog, git, is_zero, is_zero_u32, version, Result};
use super::super::{build_component_info, path_is_parent_of, ComponentGap, ContextOutput};
use serde::Serialize;
use super::resolve_git_snapshot;
use super::ComponentWithState;
use super::ContextReportStatus;
use super::validate_version_baseline_alignment;
use super::ExtensionEntry;
use super::resolve_agent_context_files;
use super::build_component_summaries;
use super::ContextReport;
use super::compute_status;
use super::compute_summary;
use super::collect_focused_components;
use super::resolve_changelog_snapshots;
use super::validate_version_targets;
use super::ProjectListItem;
use super::resolve_version_snapshot;


pub fn build_report(show_all_flag: bool, command: &str) -> Result<ContextReport> {
    let (context_output, _) = super::run(None)?;

    let relevant_ids: HashSet<String> = context_output
        .matched_components
        .iter()
        .chain(context_output.contained_components.iter())
        .cloned()
        .collect();

    let all_components = component::inventory().unwrap_or_default();
    let all_projects = project::list().unwrap_or_default();
    let all_servers = server::list().unwrap_or_default();
    let all_extensions = load_all_extensions().unwrap_or_default();

    let show_all = show_all_flag || relevant_ids.is_empty();
    let filtered_components =
        collect_focused_components(show_all, &relevant_ids, all_components, &all_projects);

    let cwd = std::env::current_dir().ok();
    let components_with_state: Vec<ComponentWithState> = filtered_components
        .into_iter()
        .map(|component| {
            let release_state = deploy::calculate_release_state(&component);
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

    let release_buckets = deploy::bucket_release_states(
        components_with_state
            .iter()
            .map(|comp| (comp.component.id.as_str(), comp.release_state.as_ref())),
    );
    let status = compute_status(&components_with_state, &release_buckets);
    let summary = compute_summary(&components_with_state);

    let linked_extension_ids: HashSet<String> = components_with_state
        .iter()
        .filter_map(|c| c.component.extensions.as_ref())
        .flat_map(|m| m.keys().cloned())
        .collect();

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

    let filtered_projects: Vec<Project> = if show_all {
        all_projects
    } else {
        all_projects
            .into_iter()
            .filter(|p| p.components.iter().any(|c| relevant_ids.contains(&c.id)))
            .collect()
    };

    let relevant_server_ids: HashSet<String> = filtered_projects
        .iter()
        .filter_map(|p| p.server_id.clone())
        .collect();

    let projects: Vec<ProjectListItem> = filtered_projects
        .into_iter()
        .map(ProjectListItem::from)
        .collect();

    let servers: Vec<Server> = if show_all {
        all_servers
    } else {
        all_servers
            .into_iter()
            .filter(|s| relevant_server_ids.contains(&s.id))
            .collect()
    };

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
    let components = build_component_summaries(&components_with_state, cwd.as_ref());

    Ok(ContextReport {
        command: command.to_string(),
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
    })
}

pub(crate) fn build_actionable_next_steps(
    status: &ContextReportStatus,
    context_output: &ContextOutput,
    components: &[ComponentWithState],
    projects: &[ProjectListItem],
    linked_extension_ids: &HashSet<String>,
    all_extensions: &[crate::extension::ExtensionManifest],
) -> Vec<String> {
    let mut next_steps = Vec::new();

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

    if status.config_gaps > 0 {
        next_steps.push(format!(
            "{} config gaps detected. Run `homeboy component show <id>` for details.",
            status.config_gaps
        ));
    }

    if context_output.managed && !components.is_empty() {
        let comp_id = &components[0].component.id;
        next_steps.push(format!(
            "You're in {}. Common: `homeboy build`, `homeboy deploy`, `homeboy release`.",
            comp_id
        ));
    }

    if Path::new("CLAUDE.md").exists() || Path::new("AGENTS.md").exists() {
        next_steps.push(
            "Read CLAUDE.md for repo-specific guidance. Run `homeboy docs commands/commands-index` for all commands.".to_string(),
        );
    }

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

    if let Some(suggestion) = context_output.suggestion.as_ref() {
        next_steps.push(format!("Suggestion: {}", suggestion));
    }

    let mut outdated_extensions = Vec::new();
    for extension in all_extensions {
        if let Some(update) = crate::extension::check_update_available(&extension.id) {
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
