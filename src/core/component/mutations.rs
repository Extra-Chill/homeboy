use crate::core::component::{
    associated_projects, inventory, mutate_portable, rename_component, resolve_effective, Component,
};
use crate::core::config;
use crate::core::error::{Error, Result};
use crate::core::output::{MergeOutput, MergeResult};
use serde_json::Value;
use std::path::Path;

/// Set the changelog target for a component's configuration.
pub fn set_changelog_target(component_id: &str, file_path: &str) -> Result<()> {
    mutate_portable(component_id, |component| {
        component.changelog_target = Some(file_path.to_string());
        Ok(())
    })?;
    Ok(())
}

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

    let updates_standalone_registration = patch
        .as_object()
        .map(|obj| obj.contains_key("local_path") || obj.contains_key("remote_path"))
        .unwrap_or(false);

    let component = mutate_portable(id, |component| {
        let fields = config::merge_config(component, patch.clone(), replace_fields)?;
        if fields.updated_fields.is_empty() {
            return Err(Error::validation_invalid_argument(
                "merge",
                "Merge patch cannot be empty",
                None,
                None,
            ));
        }
        Ok(())
    })?;

    if updates_standalone_registration {
        inventory::write_standalone_registration(&component)?;
    }

    let updated_fields = match patch {
        Value::Object(obj) => obj.keys().cloned().collect(),
        _ => vec![],
    };

    let _ = component;
    Ok(MergeOutput::Single(MergeResult {
        id: id.to_string(),
        updated_fields,
    }))
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
    fn test_set_changelog_target() {
        crate::test_support::with_isolated_home(|home| {
            let repo = write_component_repo(home, "demo-plugin");

            set_changelog_target("demo-plugin", "docs/changelog.md").expect("set changelog target");

            let config: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(repo.join("homeboy.json")).expect("read homeboy.json"),
            )
            .expect("parse homeboy.json");
            assert_eq!(
                config
                    .get("changelog_target")
                    .and_then(|value| value.as_str()),
                Some("docs/changelog.md")
            );
        });
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
            let old_repo = home.path().join("agents-api");
            let new_repo = home.path().join("agents-api-current");
            fs::create_dir_all(&old_repo).expect("old repo dir");
            fs::create_dir_all(&new_repo).expect("new repo dir");
            fs::write(
                old_repo.join("homeboy.json"),
                r#"{"id":"agents-api","remote_path":"wp-content/plugins/agents-api"}"#,
            )
            .expect("old homeboy.json");
            fs::write(
                new_repo.join("homeboy.json"),
                r#"{"id":"agents-api","remote_path":"wp-content/plugins/agents-api"}"#,
            )
            .expect("new homeboy.json");

            let component = Component::new(
                "agents-api".to_string(),
                old_repo.to_string_lossy().to_string(),
                "wp-content/plugins/agents-api".to_string(),
                None,
            );
            inventory::write_standalone_registration(&component)
                .expect("write initial standalone registration");

            let patch = serde_json::json!({
                "local_path": new_repo.to_string_lossy()
            })
            .to_string();
            let result = merge(Some("agents-api"), &patch, &[]).expect("component merge");
            let MergeOutput::Single(result) = result else {
                panic!("expected single merge result");
            };
            assert_eq!(result.updated_fields, vec!["local_path".to_string()]);

            let loaded = crate::core::component::load("agents-api").expect("load component");
            assert_eq!(loaded.local_path, new_repo.to_string_lossy());

            let registration_path = home
                .path()
                .join(".config/homeboy/components/agents-api.json");
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
}
