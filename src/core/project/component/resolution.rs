use crate::core::error::{Error, Result};
use crate::core::project::Project;
use std::collections::HashMap;
use std::path::Path;

use super::discovery::discover_attached_component;
use super::overrides::apply_component_overrides;

pub fn resolve_project_component(
    project: &Project,
    component_id: &str,
) -> Result<crate::core::component::Component> {
    resolve_project_component_with_standalone_snapshot(project, component_id, None)
}

pub fn resolve_project_component_with_standalone_snapshot(
    project: &Project,
    component_id: &str,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) -> Result<crate::core::component::Component> {
    let (mut component, attachment_local_path, attachment_remote_path) = if let Some(attachment) =
        project
            .components
            .iter()
            .find(|component| component.id == component_id)
    {
        super::super::validate_component_local_path(project, component_id)?;
        (
            discover_attached_component(Path::new(&attachment.local_path)).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "components.local_path",
                    format!(
                        "Project component '{}' points to '{}' but no homeboy.json was found",
                        component_id, attachment.local_path
                    ),
                    Some(project.id.clone()),
                    None,
                )
            })?,
            attachment.local_path.clone(),
            attachment.remote_path.clone(),
        )
    } else {
        return Err(Error::validation_invalid_argument(
            "components",
            format!(
                "Project '{}' has no attached component '{}'",
                project.id, component_id
            ),
            Some(project.id.clone()),
            None,
        ));
    };

    if let Some(remote_path) = attachment_remote_path {
        if !remote_path.trim().is_empty() {
            component.remote_path = remote_path;
        }
    }

    apply_standalone_component_fallbacks(&mut component, standalone_snapshot);

    let mut resolved = apply_component_overrides(&component, project);
    resolved.local_path = attachment_local_path;

    // Inherit project-level extensions when the component's homeboy.json doesn't
    // declare any. This handles clean tag clones from older releases where
    // extensions weren't yet in homeboy.json. (#932)
    if resolved.extensions.is_none() || resolved.extensions.as_ref().is_some_and(|e| e.is_empty()) {
        if let Some(project_extensions) = &project.extensions {
            if !project_extensions.is_empty() {
                resolved.extensions = Some(project_extensions.clone());
            }
        }
    }

    // Auto-resolve remote_path if still empty after all config layers.
    // Repo homeboy.json intentionally omits remote_path (it's deploy config),
    // so auto-detect it from source files when possible (#812).
    resolved.resolve_remote_path();

    Ok(resolved)
}

fn apply_standalone_component_fallbacks(
    component: &mut crate::core::component::Component,
    standalone_snapshot: Option<&StandaloneComponentConfigSnapshot>,
) {
    let standalone = match standalone_snapshot {
        Some(snapshot) => snapshot.get(&component.id).cloned(),
        None => load_standalone_component_config(&component.id),
    };
    let Some(standalone) = standalone else {
        return;
    };

    if component.remote_path.trim().is_empty() && !standalone.remote_path.trim().is_empty() {
        component.remote_path = standalone.remote_path;
    }

    if component.extract_command.is_none() {
        component.extract_command = standalone.extract_command;
    }

    if component.remote_url.is_none() {
        component.remote_url = standalone.remote_url;
    }
}

#[derive(Debug, Clone, Default)]
pub struct StandaloneComponentConfigSnapshot {
    components: HashMap<String, crate::core::component::Component>,
}

impl StandaloneComponentConfigSnapshot {
    pub fn load() -> Self {
        let mut snapshot = Self::default();
        let Ok(dir) = crate::core::paths::components() else {
            return snapshot;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return snapshot;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let Some(component_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(component) = load_standalone_component_config_from_path(component_id, &path)
            else {
                continue;
            };

            snapshot
                .components
                .insert(component_id.to_string(), component);
        }

        snapshot
    }

    fn get(&self, component_id: &str) -> Option<&crate::core::component::Component> {
        self.components.get(component_id)
    }
}

fn load_standalone_component_config(
    component_id: &str,
) -> Option<crate::core::component::Component> {
    let dir = crate::core::paths::components().ok()?;
    let path = dir.join(format!("{component_id}.json"));
    load_standalone_component_config_from_path(component_id, &path)
}

fn load_standalone_component_config_from_path(
    component_id: &str,
    path: &Path,
) -> Option<crate::core::component::Component> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut json: serde_json::Value = serde_json::from_str(&content).ok()?;

    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "id".to_string(),
            serde_json::Value::String(component_id.to_string()),
        );
    }

    serde_json::from_value::<crate::core::component::Component>(json).ok()
}

pub fn resolve_project_components(
    project: &Project,
) -> Result<Vec<crate::core::component::Component>> {
    let standalone_snapshot = StandaloneComponentConfigSnapshot::load();
    project
        .components
        .iter()
        .map(|component| {
            resolve_project_component_with_standalone_snapshot(
                project,
                &component.id,
                Some(&standalone_snapshot),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::project::{ProjectComponentAttachment, ProjectComponentOverrides};
    use crate::test_support::with_isolated_home;
    use tempfile::TempDir;

    fn repo_with_portable_remote_path(remote_path: &str) -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        std::fs::write(
            dir.path().join("homeboy.json"),
            serde_json::json!({
                "id": "fixture",
                "remote_path": remote_path,
                "build_artifact": "dist/fixture.zip"
            })
            .to_string(),
        )
        .expect("write homeboy.json");
        dir
    }

    fn project_with_attachment(remote_path: Option<&str>, local_path: String) -> Project {
        Project {
            id: "site".to_string(),
            components: vec![ProjectComponentAttachment {
                id: "fixture".to_string(),
                local_path,
                remote_path: remote_path.map(str::to_string),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn attachment_remote_path_overrides_portable_remote_path() {
        let repo = repo_with_portable_remote_path("../wp-content/plugins/fixture");
        let project = project_with_attachment(
            Some("wp-content/plugins/fixture"),
            repo.path().to_string_lossy().to_string(),
        );

        let component = resolve_project_component(&project, "fixture").expect("component");

        assert_eq!(component.remote_path, "wp-content/plugins/fixture");
    }

    #[test]
    fn component_overrides_still_win_over_attachment_remote_path() {
        let repo = repo_with_portable_remote_path("portable/plugins/fixture");
        let mut project = project_with_attachment(
            Some("attachment/plugins/fixture"),
            repo.path().to_string_lossy().to_string(),
        );
        project.component_overrides.insert(
            "fixture".to_string(),
            ProjectComponentOverrides {
                remote_path: Some("override/plugins/fixture".to_string()),
                ..Default::default()
            },
        );

        let component = resolve_project_component(&project, "fixture").expect("component");

        assert_eq!(component.remote_path, "override/plugins/fixture");
    }

    #[test]
    fn project_resolution_uses_standalone_extract_command_as_fallback() {
        with_isolated_home(|home| {
            let repo = repo_with_portable_remote_path("wp-content/plugins/fixture");
            let registered_repo = repo_with_portable_remote_path("wp-content/plugins/fixture");
            let components_dir = home
                .path()
                .join(".config")
                .join("homeboy")
                .join("components");
            std::fs::create_dir_all(&components_dir).expect("components dir");
            std::fs::write(
                components_dir.join("fixture.json"),
                serde_json::json!({
                    "local_path": registered_repo.path(),
                    "extract_command": "unzip -o {{artifact}} && rm {{artifact}}",
                    "remote_url": "https://github.com/example/fixture.git"
                })
                .to_string(),
            )
            .expect("standalone component config");

            let project = project_with_attachment(None, repo.path().to_string_lossy().to_string());

            let component = resolve_project_component(&project, "fixture").expect("component");

            assert_eq!(component.local_path, repo.path().to_string_lossy());
            assert_eq!(
                component.extract_command.as_deref(),
                Some("unzip -o {{artifact}} && rm {{artifact}}")
            );
            assert_eq!(
                component.remote_url.as_deref(),
                Some("https://github.com/example/fixture.git")
            );
        });
    }

    #[test]
    fn project_overrides_win_over_standalone_extract_command() {
        with_isolated_home(|home| {
            let repo = repo_with_portable_remote_path("wp-content/plugins/fixture");
            let components_dir = home
                .path()
                .join(".config")
                .join("homeboy")
                .join("components");
            std::fs::create_dir_all(&components_dir).expect("components dir");
            std::fs::write(
                components_dir.join("fixture.json"),
                serde_json::json!({
                    "local_path": repo.path(),
                    "extract_command": "unzip -o {{artifact}} && rm {{artifact}}"
                })
                .to_string(),
            )
            .expect("standalone component config");

            let mut project =
                project_with_attachment(None, repo.path().to_string_lossy().to_string());
            project.component_overrides.insert(
                "fixture".to_string(),
                ProjectComponentOverrides {
                    extract_command: Some("custom-extract {{artifact}}".to_string()),
                    ..Default::default()
                },
            );

            let component = resolve_project_component(&project, "fixture").expect("component");

            assert_eq!(
                component.extract_command.as_deref(),
                Some("custom-extract {{artifact}}")
            );
        });
    }

    #[test]
    fn status_resolution_reports_missing_component_local_path() {
        let project = project_with_attachment(
            Some("wp-content/plugins/fixture"),
            "/tmp/homeboy-missing-component-path".to_string(),
        );

        let err = resolve_project_component(&project, "fixture").expect_err("missing local_path");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("missing local_path"));
        assert!(err.hints.iter().any(|hint| {
            hint.message.contains(
                "Component 'fixture' local_path '/tmp/homeboy-missing-component-path' does not exist",
            )
        }));
    }
}
