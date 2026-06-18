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
        // An artifact upload reshaped the remote tree but the component declares
        // no version_targets, so Homeboy has no way to confirm the files actually
        // landed at remote_path. Reporting `local_version` here is precisely the
        // silent no-op bug (#3608): deploy claims success while the deployed files
        // are untouched. If the deploy went through an extension verifier
        // (`effect.verified`), trust that signal; otherwise refuse to fabricate a
        // success from an unverifiable artifact deploy.
        if effect.artifact_path.is_some() && !effect.verified {
            return Err(format!(
                "Deploy uploaded an artifact for '{}' at '{}', but the component declares no version_targets and no deploy verification, so Homeboy cannot confirm the files actually landed. Refusing to report success for an unverifiable artifact deploy. Add version_targets (e.g. the plugin's main file Version header) so post-deploy landing can be verified.",
                component.id, effect.remote_path
            ));
        }
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
                artifact_path: None,
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
                artifact_path: None,
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

    #[test]
    fn post_effect_artifact_deploy_without_version_targets_fails_loudly() {
        // Reproduces issue #3608: an artifact was uploaded and extracted, but the
        // component declares no version_targets, so Homeboy cannot confirm the
        // files actually landed. It must refuse to fabricate a success.
        let component = Component {
            id: "sample-plugin-socials".to_string(),
            remote_path: "wp-content/plugins/sample-plugin-socials".to_string(),
            version_targets: None,
            ..Component::default()
        };
        let effect = DeployEffect {
            remote_path: "wp-content/plugins/sample-plugin-socials".to_string(),
            artifact_path: Some(
                "wp-content/plugins/sample-plugin-socials/.homeboy-sample-plugin-socials.zip"
                    .to_string(),
            ),
            verified: false,
        };
        let expected_version = "0.14.0".to_string();

        let error = remote_version_after_deploy_effect(
            &component,
            &Project::default(),
            "/srv/site",
            &local_client(),
            Some(&effect),
            Some(&expected_version),
        )
        .expect_err("unverifiable artifact deploy should fail");

        assert!(
            error.contains("cannot confirm the files actually landed"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn post_effect_artifact_deploy_without_version_targets_accepts_extension_verified() {
        // When an extension verifier already confirmed the deploy (`verified`),
        // the absence of version_targets is acceptable and we report local_version.
        let component = Component {
            id: "sample-plugin-socials".to_string(),
            remote_path: "wp-content/plugins/sample-plugin-socials".to_string(),
            version_targets: None,
            ..Component::default()
        };
        let effect = DeployEffect {
            remote_path: "wp-content/plugins/sample-plugin-socials".to_string(),
            artifact_path: Some("/srv/staging/.homeboy-sample-plugin-socials.zip".to_string()),
            verified: true,
        };
        let expected_version = "0.14.0".to_string();

        let version = remote_version_after_deploy_effect(
            &component,
            &Project::default(),
            "/srv/site",
            &local_client(),
            Some(&effect),
            Some(&expected_version),
        )
        .expect("verified artifact deploy without version_targets should succeed");

        assert_eq!(version.as_deref(), Some("0.14.0"));
    }

    #[test]
    fn post_effect_no_effect_returns_local_version_without_verification() {
        // Git-style deploys produce no effect; there is no artifact-landing claim
        // to verify, so we fall back to local_version unchanged.
        let component = Component {
            id: "plugin".to_string(),
            remote_path: "plugin".to_string(),
            version_targets: None,
            ..Component::default()
        };
        let expected_version = "1.0.0".to_string();

        let version = remote_version_after_deploy_effect(
            &component,
            &Project::default(),
            "/srv/site",
            &local_client(),
            None,
            Some(&expected_version),
        )
        .expect("no-effect deploy should return local_version");

        assert_eq!(version.as_deref(), Some("1.0.0"));
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
