use crate::core::component::Component;
use crate::core::project::Project;
use crate::core::server::SshClient;

use super::types::DeployEffect;
use super::version_overrides::fetch_remote_versions_for_project;

pub(super) fn remote_version_after_deploy_effect(
    component: &Component,
    project: &Project,
    base_path: &str,
    client: &SshClient,
    effect: Option<&DeployEffect>,
    local_version: Option<&String>,
) -> std::result::Result<Option<String>, String> {
    let Some(effect) = effect else {
        return Ok(local_version.cloned());
    };
    let has_version_targets = component
        .version_targets
        .as_ref()
        .is_some_and(|targets| !targets.is_empty());

    if !has_version_targets {
        return Ok(local_version.cloned());
    }

    let observed_versions = fetch_remote_versions_for_project(
        std::slice::from_ref(component),
        Some(project),
        base_path,
        client,
    );
    let observed = observed_versions.get(&component.id).cloned().ok_or_else(|| {
        format!(
            "Deploy command completed for '{}' at '{}', but Homeboy could not read remote_version from the applied tree. Refusing to report success from stale pre-deploy observations.",
            component.id, effect.remote_path
        )
    })?;

    if let Some(expected) = local_version {
        if &observed != expected {
            return Err(format!(
                "Deploy command completed for '{}' at '{}', but post-deploy remote_version is '{}' instead of expected local_version '{}'. Refusing to report success from a mismatched deploy effect.",
                component.id, effect.remote_path, observed, expected
            ));
        }
    }

    Ok(Some(observed))
}

#[cfg(test)]
mod tests {
    use super::remote_version_after_deploy_effect;
    use crate::core::component::{Component, VersionTarget};
    use crate::core::deploy::types::DeployEffect;
    use crate::core::project::Project;
    use crate::core::server::SshClient;
    use std::collections::HashMap;

    #[test]
    fn post_effect_version_read_uses_applied_remote_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote_dir = temp.path().join("plugin");
        std::fs::create_dir_all(&remote_dir).expect("remote dir");
        std::fs::write(remote_dir.join("plugin.php"), "<?php\nVersion: 1.2.3\n")
            .expect("remote version file");
        let component = Component {
            id: "plugin".to_string(),
            remote_path: "plugin".to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
            }]),
            ..Component::default()
        };
        let effect = DeployEffect {
            remote_path: remote_dir.to_string_lossy().to_string(),
            artifact_path: Some("/tmp/plugin.zip".to_string()),
            verified: false,
        };
        let expected_version = "1.2.3".to_string();

        let version = remote_version_after_deploy_effect(
            &component,
            &Project::default(),
            temp.path().to_str().expect("base path"),
            &local_client(),
            Some(&effect),
            Some(&expected_version),
        )
        .expect("post-effect version")
        .expect("observed version");

        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn post_effect_version_read_rejects_unobserved_configured_version_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote_dir = temp.path().join("plugin");
        std::fs::create_dir_all(&remote_dir).expect("remote dir");
        let component = Component {
            id: "plugin".to_string(),
            remote_path: "plugin".to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
            }]),
            ..Component::default()
        };
        let effect = DeployEffect {
            remote_path: remote_dir.to_string_lossy().to_string(),
            artifact_path: Some("/tmp/plugin.zip".to_string()),
            verified: false,
        };
        let expected_version = "1.2.3".to_string();

        let error = remote_version_after_deploy_effect(
            &component,
            &Project::default(),
            temp.path().to_str().expect("base path"),
            &local_client(),
            Some(&effect),
            Some(&expected_version),
        )
        .expect_err("missing post-effect remote version should fail");

        assert!(
            error.contains("could not read remote_version from the applied tree"),
            "unexpected error: {error}"
        );
    }

    fn local_client() -> SshClient {
        SshClient {
            host: "localhost".to_string(),
            user: "test".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: HashMap::new(),
        }
    }
}
