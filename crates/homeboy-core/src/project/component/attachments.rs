use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use crate::component::{discover_from_portable, infer_portable_component_id};
use crate::project::{load, save, Project, ProjectComponentAttachment};

fn component_ids_from_attachments(components: &[ProjectComponentAttachment]) -> Vec<String> {
    components
        .iter()
        .map(|component| component.id.clone())
        .collect()
}

pub fn project_component_ids(project: &Project) -> Vec<String> {
    component_ids_from_attachments(&project.components)
}

pub fn has_component(project: &Project, component_id: &str) -> bool {
    project
        .components
        .iter()
        .any(|component| component.id == component_id)
}

pub fn set_component_attachments(
    project_id: &str,
    components: Vec<ProjectComponentAttachment>,
) -> Result<Vec<String>> {
    crate::config::with_config_lock(|| set_component_attachments_unlocked(project_id, components))
}

fn set_component_attachments_unlocked(
    project_id: &str,
    components: Vec<ProjectComponentAttachment>,
) -> Result<Vec<String>> {
    if components.is_empty() {
        return Err(Error::validation_invalid_argument(
            "components",
            "At least one component attachment is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut deduped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for component in components {
        if component.local_path.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "components.local_path",
                "Project component attachments require a non-empty local_path",
                Some(project_id.to_string()),
                None,
            ));
        }
        if seen.insert(component.id.clone()) {
            deduped.push(component);
        }
    }

    let mut project = load(project_id)?;
    project.components = deduped;
    save(&project)?;
    Ok(project_component_ids(&project))
}

pub fn remove_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    crate::config::with_config_lock(|| remove_components_unlocked(project_id, component_ids))
}

fn remove_components_unlocked(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    if component_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "At least one component ID is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut project = load(project_id)?;

    let mut missing = Vec::new();
    for id in &component_ids {
        if !has_component(&project, id) {
            missing.push(id.clone());
        }
    }

    if !missing.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "Component IDs not attached to project",
            Some(project_id.to_string()),
            Some(missing),
        ));
    }

    project
        .components
        .retain(|component| !component_ids.contains(&component.id));
    save(&project)?;
    Ok(project_component_ids(&project))
}

pub fn clear_component_attachments(project_id: &str) -> Result<Vec<String>> {
    crate::config::with_config_lock(|| clear_component_attachments_unlocked(project_id))
}

fn clear_component_attachments_unlocked(project_id: &str) -> Result<Vec<String>> {
    let mut project = load(project_id)?;
    project.components.clear();
    save(&project)?;
    Ok(project_component_ids(&project))
}

pub fn attach_component_path(project_id: &str, component_id: &str, local_path: &str) -> Result<()> {
    crate::config::with_config_lock_for("project components attach-path", || {
        attach_component_path_unlocked(project_id, component_id, local_path)
    })
}

fn attach_component_path_unlocked(
    project_id: &str,
    component_id: &str,
    local_path: &str,
) -> Result<()> {
    let mut project = load(project_id)?;

    let is_update = project.components.iter().any(|c| c.id == component_id);

    // When updating an existing component's path, preserve the current remote_path
    // as a project override if the new path's homeboy.json doesn't provide one.
    // This prevents clean tag clones (whose homeboy.json omits remote_path) from
    // blanking the deploy target. (#932)
    if is_update {
        preserve_remote_path_on_reattach(&mut project, component_id, local_path);
    }

    if let Some(component) = project.components.iter_mut().find(|c| c.id == component_id) {
        component.local_path = local_path.to_string();
    } else {
        project.components.push(ProjectComponentAttachment {
            id: component_id.to_string(),
            local_path: local_path.to_string(),
            ..Default::default()
        });
    }

    save(&project)
}

/// When re-attaching a component to a new path, check whether the current remote_path
/// would be lost. If the existing resolved component has a non-empty remote_path and the
/// new path's homeboy.json doesn't provide one, store the current value as a project
/// override so deploy config survives path changes.
fn preserve_remote_path_on_reattach(
    project: &mut Project,
    component_id: &str,
    new_local_path: &str,
) {
    // Already has a project-level remote_path override — nothing to preserve.
    if let Some(overrides) = project.component_overrides.get(component_id) {
        if overrides.remote_path.is_some() {
            return;
        }
    }

    // Resolve the current component to capture its remote_path.
    let current_attachment = project.components.iter().find(|c| c.id == component_id);
    let current_remote_path = current_attachment
        .and_then(|a| discover_from_portable(Path::new(&a.local_path)))
        .map(|c| c.remote_path.clone())
        .unwrap_or_default();

    if current_remote_path.trim().is_empty() {
        return;
    }

    // Check whether the new path's homeboy.json provides a remote_path.
    let new_remote_path = discover_from_portable(Path::new(new_local_path))
        .map(|c| c.remote_path.clone())
        .unwrap_or_default();

    if !new_remote_path.trim().is_empty() {
        return; // New path provides its own remote_path — no need to preserve.
    }

    // The new path would blank remote_path. Store the current value as an override.
    crate::log_status!(
        "project",
        "Preserving remote_path '{}' as project override for '{}' (new path's homeboy.json omits it)",
        current_remote_path,
        component_id
    );

    let overrides = project
        .component_overrides
        .entry(component_id.to_string())
        .or_default();
    overrides.remote_path = Some(current_remote_path);
}

pub fn attach_discovered_component_path(project_id: &str, local_path: &Path) -> Result<String> {
    crate::config::with_config_lock_for("project components attach-path", || {
        attach_discovered_component_path_unlocked(project_id, local_path)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonorepoComponentPathChange {
    pub id: String,
    pub path: String,
    pub status: MonorepoComponentPathStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonorepoComponentPathStatus {
    Attached,
    Unchanged,
    Missing,
    Unrelated,
}

/// Discovers portable components below one checkout and commits all matching
/// project-path rebases with a single config-lock acquisition and save.
pub fn rebase_monorepo_component_paths(
    project_id: &str,
    root: &Path,
    dry_run: bool,
) -> Result<Vec<MonorepoComponentPathChange>> {
    crate::config::with_config_lock(|| {
        rebase_monorepo_component_paths_unlocked(project_id, root, dry_run)
    })
}

fn rebase_monorepo_component_paths_unlocked(
    project_id: &str,
    root: &Path,
    dry_run: bool,
) -> Result<Vec<MonorepoComponentPathChange>> {
    let discovered = discover_portable_component_paths(root)?;
    let mut project = load(project_id)?;
    let root_id = infer_portable_component_id(root)?;
    let mut changes = Vec::new();
    let mut selected = HashSet::new();

    for (id, paths) in &discovered {
        if paths.len() > 1 {
            return Err(Error::validation_invalid_argument(
                "local_path",
                format!("Ambiguous component ID '{id}' appears at multiple paths"),
                Some(project_id.to_string()),
                Some(
                    paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                ),
            ));
        }
        if id == &root_id || has_component(&project, id) {
            selected.insert(id.clone());
            let path = paths[0].to_string_lossy().to_string();
            let status = project
                .components
                .iter()
                .find(|component| component.id == *id)
                .map(|component| component.local_path == path)
                .unwrap_or(false)
                .then_some(MonorepoComponentPathStatus::Unchanged)
                .unwrap_or(MonorepoComponentPathStatus::Attached);
            if status == MonorepoComponentPathStatus::Attached {
                preserve_remote_path_on_reattach(&mut project, id, &path);
                if let Some(component) = project
                    .components
                    .iter_mut()
                    .find(|component| component.id == *id)
                {
                    component.local_path = path.clone();
                } else {
                    project.components.push(ProjectComponentAttachment {
                        id: id.clone(),
                        local_path: path.clone(),
                        ..Default::default()
                    });
                }
            }
            changes.push(MonorepoComponentPathChange {
                id: id.clone(),
                path,
                status,
            });
        } else {
            changes.push(MonorepoComponentPathChange {
                id: id.clone(),
                path: paths[0].to_string_lossy().to_string(),
                status: MonorepoComponentPathStatus::Missing,
            });
        }
    }

    for component in &project.components {
        if !selected.contains(&component.id) {
            changes.push(MonorepoComponentPathChange {
                id: component.id.clone(),
                path: component.local_path.clone(),
                status: if Path::new(&component.local_path).starts_with(root) {
                    MonorepoComponentPathStatus::Missing
                } else {
                    MonorepoComponentPathStatus::Unrelated
                },
            });
        }
    }
    changes.sort_by(|left, right| left.id.cmp(&right.id).then(left.path.cmp(&right.path)));

    if !dry_run
        && changes
            .iter()
            .any(|change| change.status == MonorepoComponentPathStatus::Attached)
    {
        save(&project)?;
    }
    Ok(changes)
}

fn discover_portable_component_paths(root: &Path) -> Result<BTreeMap<String, Vec<PathBuf>>> {
    if !root.is_dir() {
        return Err(Error::validation_invalid_argument(
            "local_path",
            "Monorepo root must be an existing directory",
            Some(root.display().to_string()),
            None,
        ));
    }
    let mut pending = vec![root.to_path_buf()];
    let mut discovered = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        if directory.join("homeboy.json").is_file() {
            let id = infer_portable_component_id(&directory)?;
            discovered
                .entry(id)
                .or_insert_with(Vec::new)
                .push(directory.clone());
        }
        let mut children: Vec<_> = std::fs::read_dir(&directory)
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "local_path",
                    format!("Unable to read monorepo directory: {error}"),
                    Some(directory.display().to_string()),
                    None,
                )
            })?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name() != ".git")
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_dir())
                    .map(|_| entry.path())
            })
            .collect();
        children.sort();
        pending.extend(children.into_iter().rev());
    }
    Ok(discovered)
}

fn attach_discovered_component_path_unlocked(
    project_id: &str,
    local_path: &Path,
) -> Result<String> {
    let inferred_id = infer_portable_component_id(local_path)?;

    // When the inferred ID doesn't match any existing project component, check
    // whether a directory-name fallback produced a version-stamped ID (e.g.
    // "sample-plugin-v0402-clean" from a clone path). If an existing component
    // whose ID is a prefix of the inferred ID exists, prefer the existing ID.
    // This prevents component identity mutation from clone directory names. (#932)
    let project = load(project_id)?;
    let component_id = if has_component(&project, &inferred_id) {
        inferred_id
    } else {
        find_prefix_match(&project, &inferred_id).unwrap_or(inferred_id)
    };

    attach_component_path_unlocked(project_id, &component_id, &local_path.to_string_lossy())?;
    Ok(component_id)
}

/// Find an existing project component whose ID is a prefix of the inferred ID.
///
/// When a clean clone directory name like "sample-plugin-v0.40.2-clean" gets slugified
/// to "sample-plugin-v0402-clean", the real component ID "sample-plugin" is a prefix.
/// This function detects that pattern and returns the existing component's ID.
///
/// Only matches if:
/// - The existing ID is a proper prefix of the inferred ID
/// - The character after the prefix is a separator (dash followed by 'v' + digit,
///   or just a digit), suggesting a version/tag suffix
fn find_prefix_match(project: &Project, inferred_id: &str) -> Option<String> {
    let mut best_match: Option<&str> = None;

    for attachment in &project.components {
        let existing_id = &attachment.id;
        if inferred_id.starts_with(existing_id.as_str()) && inferred_id.len() > existing_id.len() {
            let suffix = &inferred_id[existing_id.len()..];
            // The suffix should look like a version/clone qualifier: "-v...", "-0...", etc.
            if let Some(after_dash) = suffix.strip_prefix('-') {
                let is_version_like = after_dash.starts_with('v')
                    || after_dash
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit());
                if is_version_like {
                    // Prefer the longest matching prefix (most specific existing component)
                    if best_match.is_none_or(|prev| existing_id.len() > prev.len()) {
                        best_match = Some(existing_id);
                    }
                }
            }
        }
    }

    best_match.map(|id| {
        crate::log_status!(
            "project",
            "Matched inferred ID '{}' to existing component '{}' (directory name appears version-stamped)",
            inferred_id,
            id
        );
        id.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{with_config_lock_for, CONFIG_LOCK_TIMEOUT_ENV};
    use crate::test_support::with_isolated_home;
    use std::fs;
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier};

    fn project_with_components(ids: &[&str]) -> Project {
        let mut project = Project::default();
        for id in ids {
            project.components.push(ProjectComponentAttachment {
                id: id.to_string(),
                local_path: format!("/workspace/{}", id),
                ..Default::default()
            });
        }
        project
    }

    #[test]
    fn find_prefix_match_version_suffix() {
        let project = project_with_components(&["sample-plugin", "example-theme"]);
        // Clone dir "sample-plugin-v0402-clean" → slugified inferred ID
        assert_eq!(
            find_prefix_match(&project, "sample-plugin-v0402-clean"),
            Some("sample-plugin".to_string()),
        );
    }

    #[test]
    fn find_prefix_match_numeric_suffix() {
        let project = project_with_components(&["sample-plugin"]);
        // Clone dir "sample-plugin-0402" → numeric version suffix
        assert_eq!(
            find_prefix_match(&project, "sample-plugin-0402"),
            Some("sample-plugin".to_string()),
        );
    }

    #[test]
    fn find_prefix_match_no_match_non_version_suffix() {
        let project = project_with_components(&["sample-plugin"]);
        // "sample-plugin-socials" is NOT a version suffix, it's a different component
        assert_eq!(find_prefix_match(&project, "sample-plugin-socials"), None);
    }

    #[test]
    fn find_prefix_match_exact_match_not_prefix() {
        let project = project_with_components(&["sample-plugin"]);
        // Exact match — not a prefix scenario
        assert_eq!(find_prefix_match(&project, "sample-plugin"), None);
    }

    #[test]
    fn find_prefix_match_prefers_longest() {
        let project = project_with_components(&["data", "sample-plugin"]);
        // Both "data" and "sample-plugin" are prefixes, but "sample-plugin" is longer
        assert_eq!(
            find_prefix_match(&project, "sample-plugin-v1"),
            Some("sample-plugin".to_string()),
        );
    }

    #[test]
    fn find_prefix_match_no_components() {
        let project = project_with_components(&[]);
        assert_eq!(find_prefix_match(&project, "anything-v1"), None);
    }

    #[test]
    fn concurrent_attach_discovered_component_path_preserves_all_writes() {
        with_isolated_home(|home| {
            let project = Project {
                id: "site".to_string(),
                ..Default::default()
            };
            save(&project).expect("save project");

            let repo_a = home.path().join("component-a");
            let repo_b = home.path().join("component-b");
            fs::create_dir_all(&repo_a).expect("repo a");
            fs::create_dir_all(&repo_b).expect("repo b");
            fs::write(repo_a.join("homeboy.json"), r#"{"id":"component-a"}"#)
                .expect("component a config");
            fs::write(repo_b.join("homeboy.json"), r#"{"id":"component-b"}"#)
                .expect("component b config");

            let barrier = Arc::new(Barrier::new(2));
            let handles: Vec<_> = [repo_a, repo_b]
                .into_iter()
                .map(|repo| {
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        attach_discovered_component_path("site", &repo)
                            .expect("concurrent attach succeeds");
                    })
                })
                .collect();

            for handle in handles {
                handle.join().expect("attach thread");
            }

            let mut ids = project_component_ids(&load("site").expect("load project"));
            ids.sort();
            assert_eq!(ids, vec!["component-a", "component-b"]);
        });
    }

    #[test]
    fn attach_path_reports_the_contending_lock_holder_within_the_configured_bound() {
        with_isolated_home(|home| {
            let project = Project {
                id: "site".to_string(),
                ..Default::default()
            };
            save(&project).expect("save project");

            let repo = home.path().join("component");
            fs::create_dir_all(&repo).expect("component directory");
            fs::write(repo.join("homeboy.json"), r#"{"id":"component"}"#)
                .expect("component config");

            std::env::set_var(CONFIG_LOCK_TIMEOUT_ENV, "1");
            let (holder_ready, wait_for_holder) = mpsc::sync_channel(0);
            let (release_holder, holder_release) = mpsc::sync_channel(0);
            let holder = std::thread::spawn(move || {
                with_config_lock_for("test attach-path holder", || {
                    holder_ready.send(()).expect("report lock holder");
                    holder_release.recv().expect("release lock holder");
                    Ok(())
                })
            });
            wait_for_holder.recv().expect("wait for lock holder");

            let error = attach_discovered_component_path("site", &repo)
                .expect_err("contending attach-path must time out");
            release_holder.send(()).expect("release holder");
            holder
                .join()
                .expect("holder thread")
                .expect("holder operation");
            std::env::remove_var(CONFIG_LOCK_TIMEOUT_ENV);

            assert_eq!(error.code, crate::error::ErrorCode::InternalIoError);
            assert_eq!(error.details["kind"], "config_lock_timeout");
            assert_eq!(error.details["operation"], "project components attach-path");
            assert_eq!(error.details["holder_pid"], std::process::id());
            assert_eq!(error.details["holder_operation"], "test attach-path holder");
            assert_eq!(error.details["timeout_ms"], 1_000);
            assert!(
                error.details["waited_ms"]
                    .as_u64()
                    .is_some_and(|waited| waited >= 1_000),
                "attach-path must return after the configured bound: {:?}",
                error.details
            );
        });
    }

    #[test]
    fn monorepo_rebase_previews_then_commits_all_matching_components_together() {
        with_isolated_home(|home| {
            let old = home.path().join("deleted-checkout");
            save(&Project {
                id: "site".to_string(),
                components: vec![
                    ProjectComponentAttachment {
                        id: "root".to_string(),
                        local_path: old.join("root").display().to_string(),
                        ..Default::default()
                    },
                    ProjectComponentAttachment {
                        id: "nested".to_string(),
                        local_path: old.join("nested").display().to_string(),
                        ..Default::default()
                    },
                    ProjectComponentAttachment {
                        id: "other-repo".to_string(),
                        local_path: "/other-repo".to_string(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            })
            .expect("save project");
            let root = home.path().join("checkout");
            fs::create_dir_all(root.join("nested")).expect("nested directory");
            fs::write(root.join("homeboy.json"), r#"{"id":"root"}"#).expect("root config");
            fs::write(root.join("nested/homeboy.json"), r#"{"id":"nested"}"#)
                .expect("nested config");

            let preview = rebase_monorepo_component_paths("site", &root, true).expect("preview");
            assert_eq!(
                preview
                    .iter()
                    .filter(|change| change.status == MonorepoComponentPathStatus::Attached)
                    .count(),
                2
            );
            assert_eq!(
                load("site").expect("unchanged project").components[0].local_path,
                old.join("root").display().to_string()
            );

            let applied = rebase_monorepo_component_paths("site", &root, false).expect("apply");
            assert_eq!(applied, preview);
            let project = load("site").expect("rebased project");
            assert_eq!(project.components[0].local_path, root.display().to_string());
            assert_eq!(
                project.components[1].local_path,
                root.join("nested").display().to_string()
            );
            assert_eq!(project.components[2].local_path, "/other-repo");
        });
    }

    #[test]
    fn monorepo_rebase_rejects_duplicate_ids_without_mutating_config() {
        with_isolated_home(|home| {
            let root = home.path().join("checkout");
            fs::create_dir_all(root.join("one")).expect("one directory");
            fs::create_dir_all(root.join("two")).expect("two directory");
            fs::write(root.join("homeboy.json"), r#"{"id":"root"}"#).expect("root config");
            fs::write(root.join("one/homeboy.json"), r#"{"id":"duplicate"}"#).expect("one config");
            fs::write(root.join("two/homeboy.json"), r#"{"id":"duplicate"}"#).expect("two config");
            save(&Project {
                id: "site".to_string(),
                components: vec![ProjectComponentAttachment {
                    id: "root".to_string(),
                    local_path: "/old-root".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .expect("save project");

            let error =
                rebase_monorepo_component_paths("site", &root, false).expect_err("duplicates fail");
            assert!(error.message.contains("Ambiguous component ID 'duplicate'"));
            assert_eq!(
                load("site").expect("unchanged project").components[0].local_path,
                "/old-root"
            );
        });
    }
}
