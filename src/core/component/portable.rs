use crate::core::component::{resolve_effective, Component};
use crate::core::error::{Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Read a `homeboy.json` portable config from a repo directory.
pub(crate) fn read_portable_config(repo_path: &Path) -> Result<Option<Value>> {
    let config_path = repo_path.join("homeboy.json");
    if !config_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&config_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read {}", config_path.display())),
        )
    })?;

    let value: Value = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse homeboy.json".to_string()),
            Some(content.chars().take(200).collect::<String>()),
        )
    })?;

    Ok(Some(value))
}

fn explicit_id_hints() -> Vec<String> {
    vec![
        "Add an explicit non-empty id to homeboy.json".to_string(),
        "Example: \"id\": \"my-component\"".to_string(),
    ]
}

fn portable_component_id_from_value(portable: &Value, dir: &Path) -> Result<String> {
    let id_value = portable.get("id").ok_or_else(|| {
        Error::validation_invalid_argument(
            "id",
            format!(
                "homeboy.json at {} is missing required 'id' field",
                dir.display()
            ),
            None,
            Some(explicit_id_hints()),
        )
    })?;

    let Some(id_str) = id_value.as_str() else {
        return Err(Error::validation_invalid_argument(
            "id",
            format!(
                "homeboy.json at {} must define 'id' as a non-empty string",
                dir.display()
            ),
            Some(id_value.to_string()),
            Some(vec![
                "Set id to a string such as \"my-component\"".to_string()
            ]),
        ));
    };

    if id_str.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "id",
            format!("homeboy.json at {} has a blank 'id' field", dir.display()),
            None,
            Some(explicit_id_hints()),
        ));
    }

    crate::core::engine::identifier::slugify_id(id_str, "component_id")
}

pub fn infer_portable_component_id(dir: &Path) -> Result<String> {
    let portable = read_portable_config(dir)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "local_path",
            format!("No homeboy.json found at {}", dir.display()),
            None,
            None,
        )
    })?;

    portable_component_id_from_value(&portable, dir)
}

pub fn portable_json(component: &Component) -> Result<Value> {
    // Reject blank ids before serialization (#801)
    if component.id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "id",
            "Cannot write portable config with a blank component ID",
            None,
            Some(vec![
                "Set a valid ID: homeboy component create --local-path <path>".to_string(),
            ]),
        ));
    }

    let mut value = serde_json::to_value(component).map_err(|error| {
        Error::validation_invalid_argument(
            "component",
            "Failed to serialize component to portable config",
            Some(error.to_string()),
            None,
        )
    })?;

    let obj = value.as_object_mut().ok_or_else(|| {
        Error::validation_invalid_argument(
            "component",
            "Portable component config must serialize to an object",
            None,
            None,
        )
    })?;

    obj.insert("id".to_string(), Value::String(component.id.clone()));
    obj.remove("aliases");
    obj.remove("local_path");

    Ok(value)
}

/// Write component data to the repo-local homeboy.json.
///
/// Component-owned fields round-trip through `Component`. Subsystem-owned
/// portable state is preserved only for the explicit keys below; arbitrary
/// unknown fields are dropped so stale metadata cannot masquerade as active
/// component config.
pub fn write_portable_config(dir: &Path, component: &Component) -> Result<()> {
    let path = dir.join("homeboy.json");
    let portable = portable_json(component)?;

    let merged = if path.is_file() {
        if let Ok(Some(existing)) = read_portable_config(dir) {
            merge_portable_config(existing, portable)
        } else {
            portable
        }
    } else {
        portable
    };

    validate_component_remote_urls(&merged)?;

    let content = crate::core::config::to_string_pretty(&merged)?;
    crate::core::engine::local_files::write_file_atomic(
        &path,
        &content,
        &format!("write {}", path.display()),
    )
}

pub(crate) fn validate_component_remote_urls(component: &Value) -> Result<()> {
    validate_github_remote_url_field(component, "remote_url")?;
    validate_github_remote_url_field(component, "triage_remote_url")
}

fn validate_github_remote_url_field(component: &Value, field: &str) -> Result<()> {
    let Some(value) = component.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }

    let Some(url) = value.as_str() else {
        return Err(Error::validation_invalid_argument(
            field,
            format!("Component {} must be a GitHub remote URL string", field),
            Some(value.to_string()),
            None,
        ));
    };
    if url.trim().is_empty() {
        return Ok(());
    }
    if crate::core::deploy::release_download::parse_github_url(url).is_some() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        field,
        format!("Component {} must be a GitHub remote URL", field),
        Some(url.to_string()),
        Some(vec![
            "Use https://github.com/<owner>/<repo>.git".to_string(),
            "Or use git@github.com:<owner>/<repo>.git".to_string(),
            "GitHub Enterprise hosts such as github.a8c.com are also supported".to_string(),
        ]),
    ))
}

const PORTABLE_SUBSYSTEM_KEYS: &[&str] = &["baselines", "audit_rules"];

/// Merge component fields with the explicit non-component portable owners.
fn merge_portable_config(existing: Value, component: Value) -> Value {
    let (existing, mut component) = match (existing, component) {
        (Value::Object(existing), Value::Object(component)) => (existing, component),
        (_, component) => return component,
    };

    for key in PORTABLE_SUBSYSTEM_KEYS {
        if component.contains_key(*key) {
            continue;
        }
        if let Some(value) = existing.get(*key).filter(|value| !value.is_null()) {
            component.insert((*key).to_string(), value.clone());
        }
    }

    if component
        .get("remote_path")
        .and_then(|value| value.as_str())
        .is_some_and(str::is_empty)
    {
        if let Some(value) = existing.get("remote_path").filter(|value| !value.is_null()) {
            component.insert("remote_path".to_string(), value.clone());
        }
    }

    Value::Object(component)
}

pub(crate) fn has_portable_config(path: &Path) -> bool {
    read_portable_config(path).ok().flatten().is_some()
}

pub fn mutate_portable<F>(id: &str, mutator: F) -> Result<Component>
where
    F: FnOnce(&mut Component) -> Result<()>,
{
    let mut component = resolve_effective(Some(id), None, None)?;
    let local_path = PathBuf::from(&component.local_path);

    if !has_portable_config(&local_path) {
        return Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Component '{}' does not have repo-owned homeboy.json. Initialize the repo first with `homeboy component create --local-path {}`",
                id,
                component.local_path
            ),
            Some(id.to_string()),
            None,
        ));
    }

    mutator(&mut component)?;

    write_portable_config(&local_path, &component)?;
    Ok(component)
}

/// Create a virtual (unregistered) Component from a directory's `homeboy.json`.
///
/// If the directory is a git repo and `remote_url` isn't set in the portable config,
/// auto-detects it from `git remote get-url origin`.
pub fn discover_from_portable(dir: &Path) -> Option<Component> {
    match try_discover_from_portable(dir) {
        Ok(component) => component,
        Err(error) => {
            crate::log_status!("warning", "{}", error);
            None
        }
    }
}

pub fn try_discover_from_portable(dir: &Path) -> Result<Option<Component>> {
    let Some(portable) = read_portable_config(dir)? else {
        return Ok(None);
    };

    let id = portable_component_id_from_value(&portable, dir)?;
    let local_path = dir.to_string_lossy().to_string();

    let mut json = portable;
    if let Some(obj) = json.as_object_mut() {
        obj.insert("id".to_string(), Value::String(id));
        obj.insert("local_path".to_string(), Value::String(local_path));
        obj.entry("remote_path".to_string())
            .or_insert(Value::String(String::new()));

        // Auto-detect remote_url from git if not already set
        if !obj.contains_key("remote_url") {
            if let Some(url) = crate::core::deploy::release_download::detect_remote_url(dir) {
                obj.insert("remote_url".to_string(), Value::String(url));
            }
        }
    }

    Ok(serde_json::from_value::<Component>(json).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::ComponentLabConfig;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn write_preserves_portable_subsystem_fields_and_drops_unknown_fields() {
        let dir = TempDir::new().expect("temp dir");

        let initial = serde_json::json!({
            "id": "test-comp",
            "remote_path": "wp-content/plugins/test",
            "baselines": { "audit": { "item_count": 42 } },
            "audit_rules": { "layer_rules": [] },
            "custom_field": "preserve-me"
        });
        fs::write(
            dir.path().join("homeboy.json"),
            serde_json::to_string_pretty(&initial).unwrap(),
        )
        .unwrap();

        let component = Component::new(
            "test-comp".to_string(),
            dir.path().to_string_lossy().to_string(),
            "wp-content/plugins/test".to_string(),
            None,
        );
        write_portable_config(dir.path(), &component).expect("write should succeed");

        let content = fs::read_to_string(dir.path().join("homeboy.json")).unwrap();
        let result: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            result
                .get("baselines")
                .and_then(|v| v.get("audit"))
                .and_then(|v| v.get("item_count"))
                .and_then(|v| v.as_i64()),
            Some(42),
            "baselines should be preserved"
        );
        assert_eq!(
            result
                .get("audit_rules")
                .and_then(|v| v.get("layer_rules"))
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(0),
            "audit_rules should be preserved as subsystem-owned config"
        );
        assert!(
            result.get("custom_field").is_none(),
            "unknown fields should not be preserved as active config"
        );
        assert_eq!(
            result.get("id").and_then(|v| v.as_str()),
            Some("test-comp"),
            "id should be present"
        );
    }

    #[test]
    fn write_preserves_component_lab_config() {
        let dir = TempDir::new().expect("temp dir");
        let mut component = Component::new(
            "test-comp".to_string(),
            dir.path().to_string_lossy().to_string(),
            "wp-content/plugins/test".to_string(),
            None,
        );
        component.lab = Some(ComponentLabConfig {
            self_command_prefix: vec![
                "cargo".to_string(),
                "run".to_string(),
                "--quiet".to_string(),
                "--bin".to_string(),
                "homeboy".to_string(),
                "--".to_string(),
            ],
        });

        write_portable_config(dir.path(), &component).expect("write should succeed");

        let content = fs::read_to_string(dir.path().join("homeboy.json")).unwrap();
        let result: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            result
                .get("lab")
                .and_then(|value| value.get("self_command_prefix"))
                .and_then(Value::as_array)
                .and_then(|prefix| prefix.first())
                .and_then(Value::as_str),
            Some("cargo")
        );
    }

    #[test]
    fn write_does_not_blank_remote_path() {
        let dir = TempDir::new().expect("temp dir");

        // Write homeboy.json with a real remote_path
        let initial = serde_json::json!({
            "id": "test-comp",
            "remote_path": "wp-content/plugins/test"
        });
        fs::write(
            dir.path().join("homeboy.json"),
            serde_json::to_string_pretty(&initial).unwrap(),
        )
        .unwrap();

        // Write a component with empty remote_path (simulating discover_from_portable default)
        let mut component = Component::new(
            "test-comp".to_string(),
            dir.path().to_string_lossy().to_string(),
            String::new(), // empty remote_path
            None,
        );
        component.remote_path = String::new();
        write_portable_config(dir.path(), &component).expect("write should succeed");

        // Read back — remote_path should NOT be blanked
        let content = fs::read_to_string(dir.path().join("homeboy.json")).unwrap();
        let result: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            result.get("remote_path").and_then(|v| v.as_str()),
            Some("wp-content/plugins/test"),
            "remote_path should not be blanked by an empty component value"
        );
    }

    #[test]
    fn blank_id_rejected_by_portable_json() {
        let component = Component::new(
            String::new(), // blank id
            "/tmp".to_string(),
            "/remote".to_string(),
            None,
        );
        let result = portable_json(&component);
        assert!(result.is_err(), "blank id should be rejected");
    }

    #[test]
    fn merge_config_roundtrip_preserves_component_id_regression() {
        // Regression test for #1140: component identity must be part of the typed
        // serialization round-trip so portable mutations do not need to restore it.
        let mut component = Component::new(
            "intelligence".to_string(),
            "/tmp/intelligence".to_string(),
            "wp-content/plugins/intelligence".to_string(),
            None,
        );

        let patch = serde_json::json!({ "local_path": "/new/path" });
        crate::core::config::merge_config(&mut component, patch, &[])
            .expect("merge should succeed");

        assert_eq!(component.id, "intelligence");
        assert_eq!(component.local_path, "/new/path");
    }

    #[test]
    fn blank_id_in_homeboy_json_returns_none_from_discover() {
        let dir = TempDir::new().expect("temp dir");
        let json = serde_json::json!({
            "id": "",
            "remote_path": "wp-content/plugins/test"
        });
        fs::write(
            dir.path().join("homeboy.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();

        // discover_from_portable should return None for blank id
        let result = discover_from_portable(dir.path());
        assert!(
            result.is_none(),
            "blank id should cause discover to return None"
        );
    }

    #[test]
    fn missing_id_in_homeboy_json_is_validation_error() {
        let dir = TempDir::new().expect("temp dir");
        let json = serde_json::json!({
            "remote_path": "wp-content/plugins/test"
        });
        fs::write(
            dir.path().join("homeboy.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();

        let error = infer_portable_component_id(dir.path()).expect_err("missing id must fail");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        let rendered = error.to_string();
        assert!(
            rendered.contains("missing required 'id' field"),
            "{rendered}"
        );
        assert!(
            error
                .details
                .to_string()
                .contains("Add an explicit non-empty id to homeboy.json"),
            "{}",
            error.details
        );
    }

    #[test]
    fn blank_id_in_homeboy_json_is_validation_error() {
        let dir = TempDir::new().expect("temp dir");
        let json = serde_json::json!({
            "id": "   ",
            "remote_path": "wp-content/plugins/test"
        });
        fs::write(
            dir.path().join("homeboy.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();

        let error = infer_portable_component_id(dir.path()).expect_err("blank id must fail");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        let rendered = error.to_string();
        assert!(rendered.contains("blank 'id' field"), "{rendered}");
        assert!(
            error
                .details
                .to_string()
                .contains("Add an explicit non-empty id to homeboy.json"),
            "{}",
            error.details
        );
    }

    #[test]
    fn missing_id_in_homeboy_json_returns_none_from_discover() {
        let dir = TempDir::new().expect("temp dir");
        let json = serde_json::json!({
            "remote_path": "wp-content/plugins/test"
        });
        fs::write(
            dir.path().join("homeboy.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();

        let result = discover_from_portable(dir.path());

        assert!(
            result.is_none(),
            "missing id should cause discover to return None"
        );
    }

    #[test]
    fn merge_portable_config_keeps_only_explicit_subsystem_keys() {
        let existing = serde_json::json!({
            "id": "old",
            "baselines": { "audit": {} },
            "audit_rules": { "layer_rules": [] },
            "remote_path": "real/path",
            "custom_field": "stale"
        });
        let component = serde_json::json!({
            "id": "new",
            "remote_path": "",
            "auto_cleanup": false
        });

        let merged = merge_portable_config(existing, component);

        assert_eq!(merged.get("id").and_then(|v| v.as_str()), Some("new"));
        assert!(merged.get("baselines").is_some(), "baselines preserved");
        assert!(merged.get("audit_rules").is_some(), "audit_rules preserved");
        assert!(
            merged.get("custom_field").is_none(),
            "arbitrary unknowns are dropped"
        );
        assert_eq!(
            merged.get("remote_path").and_then(|v| v.as_str()),
            Some("real/path")
        );
        assert_eq!(
            merged.get("auto_cleanup").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn write_rejects_invalid_remote_url() {
        let dir = TempDir::new().expect("temp dir");
        let mut component = Component::new(
            "test-comp".to_string(),
            dir.path().to_string_lossy().to_string(),
            String::new(),
            None,
        );
        component.remote_url = Some("/Users/chubes/Developer/homeboy".to_string());

        let result = write_portable_config(dir.path(), &component);

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().code.as_str(),
            "validation.invalid_argument"
        );
        assert!(!dir.path().join("homeboy.json").exists());
    }

    #[test]
    fn write_rejects_invalid_triage_remote_url() {
        let dir = TempDir::new().expect("temp dir");
        let mut component = Component::new(
            "test-comp".to_string(),
            dir.path().to_string_lossy().to_string(),
            String::new(),
            None,
        );
        component.triage_remote_url = Some("https://gitlab.com/foo/bar.git".to_string());

        let result = write_portable_config(dir.path(), &component);

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().code.as_str(),
            "validation.invalid_argument"
        );
        assert!(!dir.path().join("homeboy.json").exists());
    }

    #[test]
    fn test_validate_component_remote_urls_rejects_invalid_merge_without_rewriting_homeboy_json() {
        crate::test_support::with_isolated_home(|home| {
            let repo = home.path().join("my-comp");
            fs::create_dir_all(&repo).unwrap();
            let original = serde_json::json!({
                "id": "my-comp",
                "remote_url": "https://github.com/Extra-Chill/homeboy.git"
            });
            fs::write(
                repo.join("homeboy.json"),
                serde_json::to_string_pretty(&original).unwrap(),
            )
            .unwrap();

            let standalone_dir = home.path().join(".config/homeboy/components");
            fs::create_dir_all(&standalone_dir).unwrap();
            fs::write(
                standalone_dir.join("my-comp.json"),
                serde_json::json!({ "local_path": repo.to_string_lossy() }).to_string(),
            )
            .unwrap();

            let patch = r#"{"remote_url":"/Users/chubes/Developer/homeboy"}"#;
            let result = crate::core::component::mutations::merge(Some("my-comp"), patch, &[]);

            assert!(result.is_err());
            assert_eq!(
                result.unwrap_err().code.as_str(),
                "validation.invalid_argument"
            );

            let content = fs::read_to_string(repo.join("homeboy.json")).unwrap();
            let json: serde_json::Value = serde_json::from_str(&content).unwrap();
            assert_eq!(
                json.get("remote_url").and_then(|v| v.as_str()),
                Some("https://github.com/Extra-Chill/homeboy.git")
            );
        });
    }

    #[test]
    fn write_accepts_github_remote_urls() {
        let dir = TempDir::new().expect("temp dir");
        let mut component = Component::new(
            "test-comp".to_string(),
            dir.path().to_string_lossy().to_string(),
            String::new(),
            None,
        );
        component.remote_url = Some("https://github.com/Extra-Chill/homeboy.git".to_string());
        component.triage_remote_url = Some("git@github.com:Extra-Chill/homeboy.git".to_string());

        write_portable_config(dir.path(), &component).expect("GitHub remotes should be valid");

        let content = fs::read_to_string(dir.path().join("homeboy.json")).unwrap();
        let json: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            json.get("remote_url").and_then(|v| v.as_str()),
            Some("https://github.com/Extra-Chill/homeboy.git")
        );
        assert_eq!(
            json.get("triage_remote_url").and_then(|v| v.as_str()),
            Some("git@github.com:Extra-Chill/homeboy.git")
        );
    }

    #[test]
    fn write_accepts_github_enterprise_remote_urls() {
        let dir = TempDir::new().expect("temp dir");
        let mut component = Component::new(
            "test-comp".to_string(),
            dir.path().to_string_lossy().to_string(),
            String::new(),
            None,
        );
        component.remote_url = Some("git@github.a8c.com:Automattic/intelligence.git".to_string());
        component.triage_remote_url =
            Some("https://github.a8c.com/Automattic/intelligence.git".to_string());

        write_portable_config(dir.path(), &component)
            .expect("GitHub Enterprise remotes should be valid");
    }
}
