use std::collections::HashSet;

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::extension::{DeployCapability, ExtensionManifest};

/// Framework-neutral shared directory names that typically contain sibling components.
const GENERIC_PROTECTED_PATH_SUFFIXES: &[&str] =
    &["/node_modules", "/vendor", "/packages", "/extensions"];

pub(super) fn protected_path_suffixes(component: &Component) -> Vec<String> {
    let mut suffixes = HashSet::new();
    for policy in component_extension_policies(component) {
        suffixes.extend(policy.protected_path_suffixes);
    }

    let mut suffixes: Vec<String> = suffixes.into_iter().collect();
    suffixes.sort();
    suffixes
}

pub(super) fn owner_hint_for_path(component: &Component, remote_path: &str) -> Option<String> {
    component_extension_policies(component)
        .into_iter()
        .flat_map(|policy| policy.owner_hints)
        .find(|hint| remote_path.contains(&hint.path_contains))
        .map(|hint| hint.suggested_owner)
}

/// Validate that a deploy target path is safe for destructive operations.
pub(super) fn validate_deploy_target(
    install_dir: &str,
    base_path: &str,
    component_id: &str,
    extension_protected_suffixes: &[String],
) -> Result<()> {
    let normalized = install_dir.trim_end_matches('/');
    let base_normalized = base_path.trim_end_matches('/');

    if normalized == base_normalized {
        return Err(Error::validation_invalid_argument(
            "remotePath",
            format!(
                "Deploy target '{}' resolves to the project base_path — this would destroy the entire project. \
                 Set remote_path to the component's own subdirectory within the project",
                install_dir
            ),
            Some(install_dir.to_string()),
            None,
        ));
    }

    let generic_suffixes = GENERIC_PROTECTED_PATH_SUFFIXES.iter().copied();
    let extension_suffixes = extension_protected_suffixes.iter().map(String::as_str);
    for suffix in generic_suffixes.chain(extension_suffixes) {
        if normalized.ends_with(suffix.trim_end_matches('/')) {
            return Err(Error::validation_invalid_argument(
                "remotePath",
                format!(
                    "Deploy target '{}' is a shared parent directory — deploying here would delete \
                     sibling components. Set remote_path to the component's own subdirectory \
                     (e.g., '{}/{}')",
                    install_dir, normalized, component_id
                ),
                Some(install_dir.to_string()),
                None,
            ));
        }
    }

    Ok(())
}

fn component_extension_policies(component: &Component) -> Vec<DeployCapability> {
    component
        .extensions
        .as_ref()
        .into_iter()
        .flat_map(|extensions| extensions.keys())
        .filter_map(|extension_id| extension_deploy_policy(extension_id))
        .collect()
}

fn extension_deploy_policy(extension_id: &str) -> Option<DeployCapability> {
    let path = crate::core::paths::extension_manifest(extension_id).ok()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let manifest: ExtensionManifest = serde_json::from_str(&raw).ok()?;
    manifest.deploy
}

#[cfg(test)]
mod tests {
    use super::{owner_hint_for_path, protected_path_suffixes, validate_deploy_target};
    use crate::core::component::{Component, ScopedExtensionConfig};
    use crate::test_support::with_isolated_home;
    use std::collections::HashMap;

    fn write_extension_fixture(id: &str, deploy_json: &str) {
        let dir = crate::core::paths::extensions()
            .expect("extensions dir")
            .join(id);
        std::fs::create_dir_all(&dir).expect("extension dir");
        std::fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{
  "name": "{} extension",
  "version": "1.0.0",
  "deploy": {}
}}"#,
                id, deploy_json
            ),
        )
        .expect("extension manifest");
    }

    #[test]
    fn validate_deploy_target_rejects_generic_shared_suffix() {
        let result = validate_deploy_target("/srv/site/vendor", "/srv/site", "my-component", &[]);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("shared parent"));
    }

    #[test]
    fn validate_deploy_target_allows_framework_path_without_extension_policy() {
        let result = validate_deploy_target(
            "/srv/site/wp-content/plugins",
            "/srv/site",
            "my-plugin",
            &[],
        );

        assert!(result.is_ok());
    }

    #[test]
    fn validate_deploy_target_rejects_extension_protected_suffix() {
        let protected = vec!["/wp-content/plugins".to_string()];
        let result = validate_deploy_target(
            "/srv/site/wp-content/plugins",
            "/srv/site",
            "my-plugin",
            &protected,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("my-plugin"));
    }

    #[test]
    fn test_protected_path_suffixes() {
        with_isolated_home(|_| {
            write_extension_fixture(
                "example",
                r#"{
    "protected_path_suffixes": ["/shared/plugins", "/shared/uploads"]
  }"#,
            );

            let component = Component {
                id: "my-plugin".to_string(),
                extensions: Some(HashMap::from([(
                    "example".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Component::default()
            };

            assert_eq!(
                protected_path_suffixes(&component),
                vec!["/shared/plugins", "/shared/uploads"]
            );
        });
    }

    #[test]
    fn test_owner_hint_for_path() {
        with_isolated_home(|_| {
            write_extension_fixture(
                "example",
                r#"{
    "owner_hints": [
      { "path_contains": "shared/plugins/", "suggested_owner": "www-data:www-data" }
    ]
  }"#,
            );

            let component = Component {
                id: "my-plugin".to_string(),
                extensions: Some(HashMap::from([(
                    "example".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Component::default()
            };

            assert_eq!(
                owner_hint_for_path(&component, "shared/plugins/my-plugin").as_deref(),
                Some("www-data:www-data")
            );
            assert!(owner_hint_for_path(&component, "other/my-plugin").is_none());
        });
    }
}
