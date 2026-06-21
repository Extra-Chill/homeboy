use crate::core::component::{
    associated_projects, inventory, rename_component, resolve_effective, Component,
};
use crate::core::config;
use crate::core::error::{Error, Result};
use crate::core::output::{MergeOutput, MergeResult};
use crate::core::project;
use serde_json::Value;
use std::path::Path;

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    let id = id.ok_or_else(|| {
        Error::validation_invalid_argument(
            "component_id",
            "Component ID is required for component mutation",
            None,
            None,
        )
    })?;

    let raw = config::read_json_spec_to_string(json_spec)?;
    if config::is_json_array(&raw) {
        return Err(Error::validation_invalid_argument(
            "component",
            "Bulk component mutation is no longer supported. Mutate repo-owned homeboy.json one component at a time.",
            None,
            None,
        ));
    }

    let mut patch: Value = config::from_str(&raw)?;

    if let Some(json_id) = patch.get("id").and_then(|v| v.as_str()) {
        if json_id != id {
            rename(id, json_id)?;
            return merge(Some(json_id), json_spec, replace_fields);
        }
    }

    // `id` does not survive the `merge_config` serde round-trip (RawComponent.id
    // is `skip_serializing`) and would surface as a spurious "Unknown field 'id'"
    // error. Strip it here — any rename intent has already been handled above. (#1140)
    if let Value::Object(ref mut map) = patch {
        map.remove("id");
    }

    let mut component = resolve_effective(Some(id), None, None)?;
    let fields = config::merge_config(&mut component, patch.clone(), replace_fields)?;
    if fields.updated_fields.is_empty() {
        return Err(Error::validation_invalid_argument(
            "merge",
            "Merge patch cannot be empty",
            None,
            None,
        ));
    }

    if component.id.trim().is_empty() {
        component.id = id.to_string();
    }

    inventory::write_standalone_component_config(&component)?;

    if let Some(local_path) = patch.get("local_path").and_then(|value| value.as_str()) {
        update_project_attachment_local_paths(id, local_path)?;
    }

    Ok(MergeOutput::Single(MergeResult {
        id: id.to_string(),
        updated_fields: fields.updated_fields,
    }))
}

fn update_project_attachment_local_paths(component_id: &str, local_path: &str) -> Result<()> {
    if local_path.trim().is_empty() {
        return Ok(());
    }

    for project_id in associated_projects(component_id)? {
        project::attach_component_path(&project_id, component_id, local_path)?;
    }

    Ok(())
}

pub fn delete_safe(id: &str) -> Result<()> {
    let component = resolve_effective(Some(id), None, None)?;
    let local_path = Path::new(&component.local_path);
    let config_path = local_path.join("homeboy.json");

    if !config_path.exists() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!("No homeboy.json found for component '{}'", id),
            Some(id.to_string()),
            None,
        ));
    }

    if !associated_projects(id)?.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Cannot delete component '{}' while projects still reference it",
                id
            ),
            Some(id.to_string()),
            None,
        ));
    }

    std::fs::remove_file(&config_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("remove {}", config_path.display())),
        )
    })
}

pub fn rename(id: &str, new_id: &str) -> Result<Component> {
    rename_component(id, new_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_component_repo(home: &tempfile::TempDir, id: &str) -> std::path::PathBuf {
        let repo = home.path().join(id);
        fs::create_dir_all(&repo).expect("repo dir");
        fs::write(
            repo.join("homeboy.json"),
            format!(
                r#"{{"id":"{}","remote_path":"wp-content/plugins/{}"}}"#,
                id, id
            ),
        )
        .expect("homeboy.json");

        let component = Component::new(
            id.to_string(),
            repo.to_string_lossy().to_string(),
            format!("wp-content/plugins/{}", id),
            None,
        );
        inventory::write_standalone_registration(&component)
            .expect("write standalone registration");

        repo
    }

    #[test]
    fn test_delete_safe() {
        crate::test_support::with_isolated_home(|home| {
            let repo = write_component_repo(home, "demo-plugin");

            delete_safe("demo-plugin").expect("delete component config");

            assert!(!repo.join("homeboy.json").exists());
        });
    }

    #[test]
    fn test_rename() {
        crate::test_support::with_isolated_home(|home| {
            let repo = write_component_repo(home, "demo-plugin");

            let renamed = rename("demo-plugin", "renamed-plugin").expect("rename component");

            assert_eq!(renamed.id, "renamed-plugin");
            let config: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(repo.join("homeboy.json")).expect("read homeboy.json"),
            )
            .expect("parse homeboy.json");
            assert_eq!(
                config.get("id").and_then(|value| value.as_str()),
                Some("renamed-plugin")
            );
        });
    }

    #[test]
    fn merge_local_path_updates_standalone_registration() {
        crate::test_support::with_isolated_home(|home| {
            let old_repo = home.path().join("sample-plugin");
            let new_repo = home.path().join("sample-plugin-current");
            fs::create_dir_all(&old_repo).expect("old repo dir");
            fs::create_dir_all(&new_repo).expect("new repo dir");
            fs::write(
                old_repo.join("homeboy.json"),
                r#"{"id":"sample-plugin","remote_path":"wp-content/plugins/sample-plugin"}"#,
            )
            .expect("old homeboy.json");
            fs::write(
                new_repo.join("homeboy.json"),
                r#"{"id":"sample-plugin","remote_path":"wp-content/plugins/sample-plugin"}"#,
            )
            .expect("new homeboy.json");

            let component = Component::new(
                "sample-plugin".to_string(),
                old_repo.to_string_lossy().to_string(),
                "wp-content/plugins/sample-plugin".to_string(),
                None,
            );
            inventory::write_standalone_registration(&component)
                .expect("write initial standalone registration");

            let patch = serde_json::json!({
                "local_path": new_repo.to_string_lossy()
            })
            .to_string();
            let result = merge(Some("sample-plugin"), &patch, &[]).expect("component merge");
            let MergeOutput::Single(result) = result else {
                panic!("expected single merge result");
            };
            assert_eq!(result.updated_fields, vec!["local_path".to_string()]);

            let loaded = crate::core::component::load("sample-plugin").expect("load component");
            assert_eq!(loaded.local_path, new_repo.to_string_lossy());

            let registration_path = home
                .path()
                .join(".config/homeboy/components/sample-plugin.json");
            let registration: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(registration_path).expect("read registration"),
            )
            .expect("parse registration");
            assert_eq!(
                registration
                    .get("local_path")
                    .and_then(|value| value.as_str()),
                Some(new_repo.to_string_lossy().as_ref())
            );
        });
    }

    #[test]
    fn merge_writes_registry_without_rewriting_portable_config() {
        crate::test_support::with_isolated_home(|home| {
            let repo = home.path().join("sample-plugin");
            fs::create_dir_all(&repo).expect("repo dir");
            let portable = r#"{"id":"sample-plugin","remote_path":"deploy/sample-plugin"}"#;
            fs::write(repo.join("homeboy.json"), portable).expect("homeboy.json");

            let component = Component::new(
                "sample-plugin".to_string(),
                repo.to_string_lossy().to_string(),
                "deploy/sample-plugin".to_string(),
                None,
            );
            inventory::write_standalone_registration(&component)
                .expect("write initial standalone registration");

            let patch = serde_json::json!({
                "scripts": {
                    "build": ["npm run package"]
                }
            })
            .to_string();
            let result = merge(Some("sample-plugin"), &patch, &[]).expect("component merge");
            let MergeOutput::Single(result) = result else {
                panic!("expected single merge result");
            };
            assert_eq!(result.updated_fields, vec!["scripts".to_string()]);

            assert_eq!(
                fs::read_to_string(repo.join("homeboy.json")).expect("read portable config"),
                portable,
                "component set must not rewrite repo-owned homeboy.json by default"
            );

            let loaded = crate::core::component::load("sample-plugin").expect("load component");
            assert_eq!(
                loaded.scripts.expect("scripts should be registered").build,
                vec!["npm run package".to_string()]
            );

            let registration_path = home
                .path()
                .join(".config/homeboy/components/sample-plugin.json");
            let registration: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(registration_path).expect("read registration"),
            )
            .expect("parse registration");
            assert_eq!(
                registration
                    .get("scripts")
                    .and_then(|value| value.get("build"))
                    .and_then(|value| value.as_array())
                    .and_then(|commands| commands.first())
                    .and_then(|command| command.as_str()),
                Some("npm run package")
            );
        });
    }

    #[test]
    fn merge_local_path_updates_project_attachments() {
        crate::test_support::with_isolated_home(|home| {
            let old_repo = home.path().join("studio-web-old");
            let new_repo = home.path().join("studio-web-new");
            fs::create_dir_all(&old_repo).expect("old repo dir");
            fs::create_dir_all(&new_repo).expect("new repo dir");
            fs::write(
                old_repo.join("homeboy.json"),
                r#"{"id":"studio-web","remote_path":"wp-content/plugins/studio-web"}"#,
            )
            .expect("old homeboy.json");
            fs::write(
                new_repo.join("homeboy.json"),
                r#"{"id":"studio-web","remote_path":"wp-content/plugins/studio-web"}"#,
            )
            .expect("new homeboy.json");

            let component = Component::new(
                "studio-web".to_string(),
                old_repo.to_string_lossy().to_string(),
                "wp-content/plugins/studio-web".to_string(),
                None,
            );
            inventory::write_standalone_registration(&component)
                .expect("write initial standalone registration");

            let project = crate::core::project::Project {
                id: "runtime".to_string(),
                components: vec![crate::core::project::ProjectComponentAttachment {
                    id: "studio-web".to_string(),
                    local_path: old_repo.to_string_lossy().to_string(),
                    remote_path: Some("wp-content/plugins/studio-web".to_string()),
                }],
                ..Default::default()
            };
            crate::core::project::save(&project).expect("save project");

            let patch = serde_json::json!({
                "local_path": new_repo.to_string_lossy()
            })
            .to_string();
            merge(Some("studio-web"), &patch, &[]).expect("component merge");

            let loaded = crate::core::component::load("studio-web").expect("load component");
            assert_eq!(loaded.local_path, new_repo.to_string_lossy());

            let project = crate::core::project::load("runtime").expect("load project");
            assert_eq!(project.components[0].local_path, new_repo.to_string_lossy());
        });
    }
}
