//! Which deliverable a deploy addresses when a component has more than one.
//!
//! A component can be *dual-deliverable*: it builds a server-installed artifact
//! and also declares a portable deployment provider policy for a
//! provider-owned runtime. Treating the declared provider as exclusive owner
//! made the server deliverable undeployable from any project that does not
//! configure that provider's target (#12853).
//!
//! Ownership is therefore project-owned, not repository-owned: a provider is
//! selected by project target configuration
//! (`components[].deployment_provider_input`, or a project-level
//! `components[].deployment_provider` override). An operator can override the
//! inferred route explicitly with `--target`, so the choice is never forced and
//! never silent.

use homeboy_core::component::Component;
use homeboy_core::project::Project;
use serde::{Deserialize, Serialize};

use super::types::DeployConfig;

/// The deliverable a deploy addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployTarget {
    /// The build artifact installed at the project's remote path.
    Server,
    /// The target owned by the component's declared deployment provider.
    Provider,
}

impl DeployTarget {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Provider => "provider",
        }
    }
}

impl std::fmt::Display for DeployTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Resolve the target one attached component is deployed to.
///
/// An explicit `--target` wins outright. `--target provider` deliberately routes
/// to the provider even when the project selects no provider target, so the
/// provider lifecycle reports precisely what is missing instead of this function
/// guessing on the operator's behalf.
pub(super) fn resolve(
    project: &Project,
    component_id: &str,
    config: &DeployConfig,
) -> DeployTarget {
    if let Some(target) = config.target {
        return target;
    }
    if project
        .components
        .iter()
        .find(|attachment| attachment.id == component_id)
        .is_some_and(|attachment| attachment.selects_deployment_provider())
    {
        DeployTarget::Provider
    } else {
        DeployTarget::Server
    }
}

/// Disclose the route a server-deployed component took.
///
/// Returns `None` for a component with a single deliverable, where the route is
/// not a choice and saying so is noise. A dual-deliverable component always
/// gets a line: it is the case where an operator can be surprised, so it must
/// never be silent, and the line names the flag that selects the other target.
pub(super) fn server_route_disclosure(
    component: &Component,
    project: &Project,
    config: &DeployConfig,
) -> Option<String> {
    let provider = component.deployment_provider.as_ref()?;
    let selected = project
        .components
        .iter()
        .find(|attachment| attachment.id == component.id)
        .is_some_and(|attachment| attachment.selects_deployment_provider());
    let reason = match (config.target, selected) {
        (Some(DeployTarget::Server), true) => {
            "--target server overrides this project's selected provider target"
        }
        (Some(DeployTarget::Server), false) => "--target server was requested",
        _ => "this project selects no provider target",
    };
    Some(format!(
        "deployment route: server ({reason}); component also declares deployment provider '{}' from extension '{}' — select it with --target provider after setting components.deployment_provider_input for this project",
        provider.provider, provider.extension
    ))
}

#[cfg(test)]
mod tests {
    use super::{resolve, server_route_disclosure, DeployTarget};
    use crate::DeployConfig;
    use homeboy_core::component::{Component, DeploymentProviderAttachment};
    use homeboy_core::project::{Project, ProjectComponentAttachment};

    fn dual_deliverable_component() -> Component {
        let mut component = Component::new(
            "wp-codebox".to_string(),
            "/source/wp-codebox".to_string(),
            "packages/wordpress-plugin/dist/wp-codebox.zip".to_string(),
            None,
        );
        component.deployment_provider = Some(DeploymentProviderAttachment {
            extension: "cloudflare-workers".to_string(),
            provider: "cloudflare-workers.deploy".to_string(),
            contract: None,
            policy: Some(serde_json::json!({ "repository": "shared" })),
        });
        component
    }

    fn project(input: Option<serde_json::Value>) -> Project {
        Project {
            id: "extrachill-site".to_string(),
            components: vec![ProjectComponentAttachment {
                id: "wp-codebox".to_string(),
                local_path: "/source/wp-codebox".to_string(),
                remote_path: Some("wp-content/plugins/wp-codebox".to_string()),
                deployment_provider_input: input,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn config(target: Option<DeployTarget>) -> DeployConfig {
        let mut config = DeployConfig::check_all_no_pull_head();
        config.target = target;
        config
    }

    /// An unselected provider leaves the server deliverable deployable
    /// (#12853), project target configuration selects the provider, and an
    /// explicit `--target` is never re-decided.
    #[test]
    fn resolve_routes_by_explicit_target_then_project_selection() {
        let selected = || Some(serde_json::json!({ "target": "site" }));
        let cases = [
            (None, None, DeployTarget::Server),
            (selected(), None, DeployTarget::Provider),
            (None, Some(DeployTarget::Provider), DeployTarget::Provider),
            (selected(), Some(DeployTarget::Server), DeployTarget::Server),
        ];

        for (input, target, expected) in cases {
            assert_eq!(
                resolve(&project(input.clone()), "wp-codebox", &config(target)),
                expected,
                "input={input:?} target={target:?}"
            );
        }
    }

    #[test]
    fn server_route_disclosure_names_the_other_deliverable_only_when_dual() {
        let disclosure =
            server_route_disclosure(&dual_deliverable_component(), &project(None), &config(None))
                .expect("a dual-deliverable route must be disclosed");
        assert!(disclosure.contains("this project selects no provider target"));
        assert!(disclosure.contains("'cloudflare-workers.deploy'"));
        assert!(disclosure.contains("--target provider"));

        let overridden = server_route_disclosure(
            &dual_deliverable_component(),
            &project(Some(serde_json::json!({ "target": "site" }))),
            &config(Some(DeployTarget::Server)),
        )
        .expect("an overridden route must be disclosed");
        assert!(overridden.contains("overrides this project's selected provider target"));

        let single = Component::new(
            "plugin".to_string(),
            "/source/plugin".to_string(),
            "dist/plugin.zip".to_string(),
            None,
        );
        assert!(server_route_disclosure(&single, &project(None), &config(None)).is_none());
    }
}
