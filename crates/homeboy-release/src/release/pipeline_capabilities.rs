use super::context::ReleaseExtension;

/// Derive publish targets from extensions that have `release.publish` action.
pub(super) fn get_publish_targets(extensions: &[ReleaseExtension]) -> Vec<String> {
    extensions
        .iter()
        .filter(|extension| extension.provides_action("release.publish"))
        .map(|m| m.id.clone())
        .collect()
}

/// Check if any extension provides the `release.package` action.
pub(super) fn has_package_capability(extensions: &[ReleaseExtension]) -> bool {
    extensions
        .iter()
        .any(|extension| extension.provides_action("release.package"))
}

/// Check if any extension provides the `release.prepare` action.
pub(super) fn has_prepare_capability(extensions: &[ReleaseExtension]) -> bool {
    extensions
        .iter()
        .any(|extension| extension.provides_action("release.prepare"))
}

#[cfg(test)]
mod tests {
    use super::{get_publish_targets, has_package_capability, has_prepare_capability};
    use crate::release::context::ReleaseExtension;
    use homeboy_extension_contract::ExtensionManifest;

    fn extension(id: &str, actions: &[&str]) -> ReleaseExtension {
        let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": id,
            "version": "1.0.0",
            "actions": actions.iter().map(|action| serde_json::json!({
                "id": action,
                "label": action,
                "type": "command",
                "command": "true"
            })).collect::<Vec<_>>()
        }))
        .expect("extension manifest");
        manifest.id = id.to_string();
        ReleaseExtension::from_manifest(&manifest)
    }

    #[test]
    fn release_capabilities_detect_prepare_package_and_publish_actions() {
        let extensions = vec![
            extension("rust", &["release.prepare", "release.publish"]),
            extension("dist", &["release.package"]),
        ];

        assert!(has_prepare_capability(&extensions));
        assert!(has_package_capability(&extensions));
        assert_eq!(get_publish_targets(&extensions), vec!["rust"]);
    }
}
