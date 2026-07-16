//! Target-only binding for immutable component payloads.
//!
//! This layer deliberately does not resolve source, build, contact a target, or
//! retain payload resources. Callers keep process-local payload ownership alive.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::component::Component;
use crate::error::{Error, Result};
use crate::project::Project;

use super::path_roots::resolve_effective_remote_path;
use super::policy::{protected_path_suffixes, validate_deploy_target};
use super::PreparedDeployArtifact;

#[derive(Debug, Clone)]
pub(crate) struct ProjectPayloadBinding {
    pub component: Component,
    pub artifact: PreparedDeployArtifact,
    pub install_dir: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectPayloadBindingEvidence {
    pub project_id: String,
    pub component_id: String,
    pub install_dir: String,
    pub strategy: String,
    pub install: InstallInstructions,
    pub artifact: PayloadIdentityEvidence,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct InstallInstructions {
    pub extract_command: Option<String>,
    pub remote_owner: Option<String>,
    pub cli_path: Option<String>,
    pub hooks: std::collections::HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PayloadIdentityEvidence {
    pub component_id: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub version: String,
    pub tag: String,
    pub source_commit: String,
}

impl ProjectPayloadBinding {
    pub fn evidence(&self, project_id: &str) -> ProjectPayloadBindingEvidence {
        ProjectPayloadBindingEvidence {
            project_id: project_id.to_string(),
            component_id: self.component.id.clone(),
            install_dir: self.install_dir.clone(),
            strategy: self
                .component
                .deploy_strategy()
                .unwrap_or("rsync")
                .to_string(),
            install: InstallInstructions {
                extract_command: self.component.extract_command.clone(),
                remote_owner: self.component.remote_owner.clone(),
                cli_path: self.component.cli_path.clone(),
                hooks: self.component.hooks.clone(),
            },
            artifact: PayloadIdentityEvidence {
                component_id: self.artifact.component_id.clone(),
                size_bytes: self.artifact.size_bytes,
                sha256: self.artifact.sha256.clone(),
                version: self.artifact.version.clone(),
                tag: self.artifact.tag.clone(),
                source_commit: self.artifact.source_commit.clone(),
            },
        }
    }
}

/// Bind already-prepared artifacts to one project's effective install policy.
/// No filesystem, source, network, lifecycle, transfer, or install effect occurs here.
pub(crate) fn bind_project_payloads(
    project: &Project,
    base_path: &str,
    components: &[Component],
    payloads: &HashMap<String, PreparedDeployArtifact>,
) -> Result<Vec<ProjectPayloadBinding>> {
    validate_deploy_together(components)?;

    components
        .iter()
        .map(|component| {
            let artifact = payloads.get(&component.id).cloned().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "prepared_artifact",
                    format!(
                        "No prepared payload exists for component '{}'",
                        component.id
                    ),
                    None,
                    None,
                )
            })?;
            if artifact.component_id != component.id {
                return Err(Error::validation_invalid_argument(
                    "prepared_artifact.component_id",
                    format!(
                        "Prepared artifact is for '{}' rather than '{}'",
                        artifact.component_id, component.id
                    ),
                    None,
                    None,
                ));
            }
            match component.deploy_strategy() {
                Some("git" | "file") => {
                    return Err(Error::validation_invalid_argument(
                        "deploy_strategy",
                        "Prepared artifacts require an artifact deploy strategy",
                        None,
                        None,
                    ));
                }
                _ => {}
            }
            let install_dir = resolve_effective_remote_path(project, component, base_path)?;
            validate_deploy_target(
                &install_dir,
                base_path,
                &component.id,
                &protected_path_suffixes(component),
            )?;
            Ok(ProjectPayloadBinding {
                component: component.clone(),
                artifact,
                install_dir,
            })
        })
        .collect()
}

fn validate_deploy_together(components: &[Component]) -> Result<()> {
    let selected = components
        .iter()
        .map(|component| component.id.as_str())
        .collect::<HashSet<_>>();
    let mut missing = Vec::new();
    for component in components {
        for companion in &component.deploy_together {
            if companion != &component.id && !selected.contains(companion.as_str()) {
                missing.push(format!("{} requires {}", component.id, companion));
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        missing.sort();
        Err(Error::validation_invalid_argument(
            "deploy_together",
            "Deploy selection omits coupled components",
            None,
            Some(missing),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(id: &str, remote_path: &str) -> Component {
        Component {
            id: id.to_string(),
            remote_path: remote_path.to_string(),
            ..Component::default()
        }
    }

    fn payload(id: &str) -> PreparedDeployArtifact {
        PreparedDeployArtifact {
            component_id: id.to_string(),
            path: "/private/tmp/payload.zip".to_string(),
            durable_path: "/private/tmp/payload.zip".to_string(),
            size_bytes: 7,
            sha256: "hash".to_string(),
            version: "1.0.0".to_string(),
            tag: "v1.0.0".to_string(),
            source_commit: "commit".to_string(),
        }
    }

    fn project(id: &str) -> Project {
        Project {
            id: id.to_string(),
            ..Project::default()
        }
    }

    #[test]
    fn binds_one_payload_to_two_project_paths_without_changing_identity() {
        let component = component("fixture", "plugins/one");
        let payload = payload("fixture");
        let payloads = HashMap::from([("fixture".to_string(), payload.clone())]);
        let first = bind_project_payloads(
            &project("first"),
            "/srv/first",
            std::slice::from_ref(&component),
            &payloads,
        )
        .expect("first binding");
        let mut second_component = component;
        second_component.remote_path = "plugins/two".to_string();
        let second = bind_project_payloads(
            &project("second"),
            "/srv/second",
            &[second_component],
            &payloads,
        )
        .expect("second binding");

        assert_eq!(first[0].artifact, payload);
        assert_eq!(second[0].artifact, payload);
        assert_eq!(first[0].install_dir, "/srv/first/plugins/one");
        assert_eq!(second[0].install_dir, "/srv/second/plugins/two");
    }

    #[test]
    fn rejects_invalid_bindings_before_effects() {
        let mut coupled = component("one", "plugins/one");
        coupled.deploy_together = vec!["two".to_string()];
        let payloads = HashMap::from([("one".to_string(), payload("one"))]);
        assert!(
            bind_project_payloads(&project("site"), "/srv/site", &[coupled], &payloads)
                .unwrap_err()
                .to_string()
                .contains("omits coupled")
        );

        let unsafe_component = component("one", "../outside");
        assert!(bind_project_payloads(
            &project("site"),
            "/srv/site",
            &[unsafe_component],
            &payloads
        )
        .is_err());

        assert!(bind_project_payloads(
            &project("site"),
            "/srv/site",
            &[component("one", "")],
            &payloads
        )
        .is_err());

        let mut git_component = component("one", "plugins/one");
        git_component.deploy_strategy = Some("git".to_string());
        assert!(
            bind_project_payloads(&project("site"), "/srv/site", &[git_component], &payloads)
                .unwrap_err()
                .to_string()
                .contains("artifact deploy strategy")
        );

        let mismatch = HashMap::from([("one".to_string(), payload("other"))]);
        assert!(bind_project_payloads(
            &project("site"),
            "/srv/site",
            &[component("one", "plugins/one")],
            &mismatch
        )
        .unwrap_err()
        .to_string()
        .contains("rather than"));
    }

    #[test]
    fn evidence_excludes_payload_paths() {
        let payloads = HashMap::from([("fixture".to_string(), payload("fixture"))]);
        let binding = bind_project_payloads(
            &project("site"),
            "/srv/site",
            &[component("fixture", "plugins/fixture")],
            &payloads,
        )
        .expect("binding");
        let evidence = serde_json::to_string(&binding[0].evidence("site")).expect("evidence");

        assert!(!evidence.contains("/private/tmp"));
        assert!(!evidence.contains("durable_path"));
    }
}
