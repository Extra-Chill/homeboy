use homeboy_core::component::Component;
use homeboy_core::project::Project;
use homeboy_core::server::SshClient;
use std::time::Duration;

use super::types::DeployEffect;
use super::version_overrides::fetch_remote_versions_for_project;

const POST_DEPLOY_VERSION_PROBE_ATTEMPTS: usize = 3;
const POST_DEPLOY_VERSION_PROBE_SETTLE_DELAY: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub(super) enum PostDeployVerification {
    Verified(Option<String>),
    AppliedUnverified(String),
}

pub(super) fn remote_version_after_deploy_effect(
    component: &Component,
    project: &Project,
    base_path: &str,
    client: &SshClient,
    effect: Option<&DeployEffect>,
    local_version: Option<&String>,
) -> std::result::Result<PostDeployVerification, String> {
    let Some(effect) = effect else {
        return Ok(PostDeployVerification::Verified(local_version.cloned()));
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
        return Ok(PostDeployVerification::Verified(local_version.cloned()));
    }

    let Some(observed) =
        observe_remote_version_from_applied_tree(component, project, base_path, client)
    else {
        return Ok(PostDeployVerification::AppliedUnverified(format!(
            "Deploy command completed for '{}' at '{}', and Homeboy retried post-deploy remote_version verification against the applied tree, but the postcondition remained unreadable. Artifact mutation evidence is preserved; reverify with 'homeboy deploy --project {} --component {} --check'.",
            component.id, effect.remote_path, project.id, component.id
        )));
    };

    if let Some(expected) = local_version {
        if &observed != expected {
            return Err(format!(
                "Deploy command completed for '{}' at '{}', but post-deploy remote_version is '{}' instead of expected local_version '{}'. Refusing to report success from a mismatched deploy effect.",
                component.id, effect.remote_path, observed, expected
            ));
        }
    }

    Ok(PostDeployVerification::Verified(Some(observed)))
}

fn observe_remote_version_from_applied_tree(
    component: &Component,
    project: &Project,
    base_path: &str,
    client: &SshClient,
) -> Option<String> {
    for attempt in 0..POST_DEPLOY_VERSION_PROBE_ATTEMPTS {
        let observed_versions = fetch_remote_versions_for_project(
            std::slice::from_ref(component),
            Some(project),
            base_path,
            client,
        )
        .versions;
        if let Some(observed) = observed_versions.get(&component.id).cloned() {
            return Some(observed);
        }
        if attempt + 1 < POST_DEPLOY_VERSION_PROBE_ATTEMPTS {
            std::thread::sleep(POST_DEPLOY_VERSION_PROBE_SETTLE_DELAY);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{remote_version_after_deploy_effect, PostDeployVerification};
    use crate::test_support::local_client;
    use crate::types::DeployEffect;
    use homeboy_core::component::{Component, VersionTarget};
    use homeboy_core::project::Project;

    fn versioned_plugin() -> Component {
        Component {
            id: "plugin".to_string(),
            remote_path: "plugin".to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                artifact_path: None,
            }]),
            ..Component::default()
        }
    }

    #[test]
    fn post_effect_version_reads_applied_tree_and_degrades_to_applied_unverified() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote_dir = temp.path().join("plugin");
        std::fs::create_dir_all(&remote_dir).expect("remote dir");
        let effect = DeployEffect {
            remote_path: remote_dir.to_string_lossy().to_string(),
            artifact_path: Some("/tmp/plugin.zip".to_string()),
            verified: false,
        };
        let project = Project {
            id: "site".to_string(),
            ..Project::default()
        };
        let expected_version = "1.2.3".to_string();
        let verify = || {
            remote_version_after_deploy_effect(
                &versioned_plugin(),
                &project,
                temp.path().to_str().expect("base path"),
                &local_client(),
                Some(&effect),
                Some(&expected_version),
            )
        };

        let PostDeployVerification::AppliedUnverified(message) =
            verify().expect("unreadable probe should stay typed")
        else {
            panic!("expected applied_unverified outcome");
        };
        assert!(
            message.contains("homeboy deploy --project site --component plugin --check"),
            "unexpected outcome: {message}"
        );

        std::fs::write(remote_dir.join("plugin.php"), "<?php\nVersion: 1.2.3\n")
            .expect("remote version file");
        let PostDeployVerification::Verified(Some(version)) =
            verify().expect("post-effect version")
        else {
            panic!("expected verified post-effect version");
        };
        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn post_effect_without_version_targets_requires_verifier_for_artifact_deploys() {
        // Issue #3608: an artifact deploy with no version_targets and no
        // extension verifier must not fabricate success. Verified artifact
        // deploys and effect-less (git) deploys report local_version.
        let component = Component {
            id: "plugin".to_string(),
            remote_path: "wp-content/plugins/plugin".to_string(),
            version_targets: None,
            ..Component::default()
        };
        let artifact_effect = |verified| DeployEffect {
            remote_path: "wp-content/plugins/plugin".to_string(),
            artifact_path: Some("/srv/staging/.homeboy-plugin.zip".to_string()),
            verified,
        };
        let expected_version = "0.14.0".to_string();
        let verify = |effect: Option<&DeployEffect>| {
            remote_version_after_deploy_effect(
                &component,
                &Project::default(),
                "/srv/site",
                &local_client(),
                effect,
                Some(&expected_version),
            )
        };

        let error = verify(Some(&artifact_effect(false)))
            .expect_err("unverifiable artifact deploy should fail");
        assert!(
            error.contains("cannot confirm the files actually landed"),
            "unexpected error: {error}"
        );

        for effect in [Some(artifact_effect(true)), None] {
            let Ok(PostDeployVerification::Verified(version)) = verify(effect.as_ref()) else {
                panic!("expected verified outcome for {effect:?}");
            };
            assert_eq!(version.as_deref(), Some("0.14.0"));
        }
    }
}
