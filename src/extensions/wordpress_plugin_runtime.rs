use std::path::Path;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingComposerAutoloadRuntime {
    pub required_path: String,
}

pub(crate) fn missing_composer_autoload_runtime(
    plugin_root: &Path,
) -> Option<MissingComposerAutoloadRuntime> {
    let composer_path = plugin_root.join("composer.json");
    if !composer_path.is_file() || plugin_root.join("vendor/autoload.php").is_file() {
        return None;
    }

    let manifest = std::fs::read_to_string(composer_path).ok()?;
    let manifest: Value = serde_json::from_str(&manifest).ok()?;
    if !composer_declares_runtime_autoload(&manifest) {
        return None;
    }

    Some(MissingComposerAutoloadRuntime {
        required_path: "vendor/autoload.php".to_string(),
    })
}

fn composer_declares_runtime_autoload(manifest: &Value) -> bool {
    manifest
        .get("autoload")
        .is_some_and(composer_section_has_entries)
}

fn composer_section_has_entries(section: &Value) -> bool {
    match section {
        Value::Object(entries) => !entries.is_empty(),
        Value::Array(entries) => !entries.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_missing_vendor_autoload_for_composer_runtime_autoload() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("composer.json"),
            r#"{
                "autoload": {
                    "classmap": ["includes/"]
                }
            }"#,
        )
        .unwrap();

        let missing = missing_composer_autoload_runtime(temp.path())
            .expect("autoloaded plugin source should require vendor/autoload.php");

        assert_eq!(missing.required_path, "vendor/autoload.php");
    }

    #[test]
    fn accepts_runtime_complete_composer_autoload() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("vendor")).unwrap();
        std::fs::write(temp.path().join("vendor/autoload.php"), "<?php\n").unwrap();
        std::fs::write(
            temp.path().join("composer.json"),
            r#"{"autoload":{"psr-4":{"Example\\":"src/"}}}"#,
        )
        .unwrap();

        assert!(missing_composer_autoload_runtime(temp.path()).is_none());
    }

    #[test]
    fn ignores_composer_without_runtime_autoload() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("composer.json"),
            r#"{"autoload-dev":{"psr-4":{"Example\\Tests\\":"tests/"}}}"#,
        )
        .unwrap();

        assert!(missing_composer_autoload_runtime(temp.path()).is_none());
    }
}
