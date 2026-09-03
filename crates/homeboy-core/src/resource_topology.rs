//! Read-only resolution of declared resource relationships.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use homeboy_resource_topology_contract::{
    ResourceTopologyDiagnostic, ResourceTopologyEdge, ResourceTopologyEdgeKind,
    ResourceTopologyResourceKind, ResourceTopologyResourceRef, ResourceTopologySnapshot,
    RESOURCE_TOPOLOGY_SNAPSHOT_SCHEMA,
};

use crate::{component, fleet, project, server, Error, Result};

/// Runner identity and its declared backing server, supplied by the runner subsystem.
///
/// Core intentionally does not load runners itself: Runner configuration and its
/// `RunnerSpec` conversion seam are owned by `homeboy-lab-runner`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceTopologyRunner {
    pub id: String,
    pub server_id: Option<String>,
}

/// Resolve roots using the process configuration root.
pub fn resolve(
    roots: &[ResourceTopologyResourceRef],
    runners: &[ResourceTopologyRunner],
) -> Result<ResourceTopologySnapshot> {
    resolve_in_root(&crate::paths::homeboy()?, roots, runners)
}

/// Resolve roots from one explicit configuration root.
pub fn resolve_in_root(
    config_root: &Path,
    roots: &[ResourceTopologyResourceRef],
    runners: &[ResourceTopologyRunner],
) -> Result<ResourceTopologySnapshot> {
    let runners = runners
        .iter()
        .map(|runner| (runner.id.clone(), runner.server_id.clone()))
        .collect();
    let mut resolver = Resolver {
        config_root,
        runners,
        resources: BTreeSet::new(),
        edges: BTreeSet::new(),
        diagnostics: BTreeSet::new(),
        visited: BTreeSet::new(),
    };
    let roots = roots.iter().cloned().collect::<BTreeSet<_>>();

    for root in &roots {
        resolver.visit_root(root)?;
    }

    Ok(ResourceTopologySnapshot {
        schema: RESOURCE_TOPOLOGY_SNAPSHOT_SCHEMA.to_string(),
        resources: resolver.resources.into_iter().collect(),
        edges: resolver.edges.into_iter().collect(),
        roots: roots.into_iter().collect(),
        diagnostics: resolver.diagnostics.into_iter().collect(),
    })
}

struct Resolver<'a> {
    config_root: &'a Path,
    runners: BTreeMap<String, Option<String>>,
    resources: BTreeSet<ResourceTopologyResourceRef>,
    edges: BTreeSet<ResourceTopologyEdge>,
    diagnostics: BTreeSet<ResourceTopologyDiagnostic>,
    visited: BTreeSet<ResourceTopologyResourceRef>,
}

impl Resolver<'_> {
    fn visit_root(&mut self, root: &ResourceTopologyResourceRef) -> Result<()> {
        self.visit(root)
    }

    fn visit(&mut self, resource: &ResourceTopologyResourceRef) -> Result<()> {
        if !self.visited.insert(resource.clone()) {
            return Ok(());
        }

        match resource.kind {
            ResourceTopologyResourceKind::Component => {
                component::load_in_root(self.config_root, &resource.id)?;
            }
            ResourceTopologyResourceKind::Project => {
                let project = project::load_in_root(self.config_root, &resource.id)?;
                self.resources.insert(resource.clone());
                self.visit_project(resource, &project);
                return Ok(());
            }
            ResourceTopologyResourceKind::Server => {
                server::load_in_root(self.config_root, &resource.id)?;
            }
            ResourceTopologyResourceKind::Fleet => {
                let fleet = fleet::load_in_root(self.config_root, &resource.id)?;
                self.resources.insert(resource.clone());
                self.visit_fleet(resource, &fleet);
                return Ok(());
            }
            ResourceTopologyResourceKind::Runner => {
                let server_id = self.runners.get(&resource.id).ok_or_else(|| {
                    Error::runner_not_found(
                        resource.id.clone(),
                        self.runners.keys().cloned().collect(),
                    )
                })?;
                self.resources.insert(resource.clone());
                if let Some(server_id) = server_id.clone() {
                    self.visit_declared(
                        resource,
                        ResourceTopologyEdgeKind::RunnerUsesServer,
                        ResourceTopologyResourceKind::Server,
                        server_id,
                    );
                }
                return Ok(());
            }
        }

        self.resources.insert(resource.clone());
        Ok(())
    }

    fn visit_fleet(&mut self, fleet_ref: &ResourceTopologyResourceRef, fleet: &fleet::Fleet) {
        let project_ids = fleet.project_ids.iter().cloned().collect::<BTreeSet<_>>();
        for project_id in project_ids {
            self.visit_declared(
                fleet_ref,
                ResourceTopologyEdgeKind::FleetContainsProject,
                ResourceTopologyResourceKind::Project,
                project_id,
            );
        }
    }

    fn visit_project(
        &mut self,
        project_ref: &ResourceTopologyResourceRef,
        project: &project::Project,
    ) {
        if let Some(server_id) = project.server_id.clone() {
            self.visit_declared(
                project_ref,
                ResourceTopologyEdgeKind::ProjectTargetsServer,
                ResourceTopologyResourceKind::Server,
                server_id,
            );
        }

        for component_id in project::project_component_ids(project)
            .into_iter()
            .collect::<BTreeSet<_>>()
        {
            self.visit_declared(
                project_ref,
                ResourceTopologyEdgeKind::ProjectUsesComponent,
                ResourceTopologyResourceKind::Component,
                component_id,
            );
        }
    }

    fn visit_declared(
        &mut self,
        declared_by: &ResourceTopologyResourceRef,
        edge_kind: ResourceTopologyEdgeKind,
        kind: ResourceTopologyResourceKind,
        id: String,
    ) {
        let reference = ResourceTopologyResourceRef { kind, id };
        if self.visit(&reference).is_ok() {
            self.edges.insert(ResourceTopologyEdge {
                kind: edge_kind,
                from: declared_by.clone(),
                to: reference,
            });
        } else {
            self.diagnostics
                .insert(ResourceTopologyDiagnostic::UnresolvedReference {
                    reference,
                    declared_by: declared_by.clone(),
                    edge_kind,
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(kind: ResourceTopologyResourceKind, id: &str) -> ResourceTopologyResourceRef {
        ResourceTopologyResourceRef {
            kind,
            id: id.to_string(),
        }
    }

    #[test]
    fn resolves_a_partial_fleet_in_stable_order() {
        crate::test_support::with_isolated_home(|_| {
            let project_config = crate::paths::project_config("site").expect("project path");
            std::fs::create_dir_all(project_config.parent().expect("project directory"))
                .expect("project directory");
            std::fs::write(
                project_config,
                r#"{"server_id":"missing-server","components":[{"id":"missing-component","local_path":"/source/component"}]}"#,
            )
            .expect("legacy partial project config");
            fleet::save(&fleet::Fleet::new(
                "production".to_string(),
                vec!["missing-project".to_string(), "site".to_string()],
            ))
            .expect("fleet config");

            let snapshot = resolve(
                &[resource(ResourceTopologyResourceKind::Fleet, "production")],
                &[],
            )
            .expect("topology");

            assert_eq!(
                snapshot.resources,
                vec![
                    resource(ResourceTopologyResourceKind::Project, "site"),
                    resource(ResourceTopologyResourceKind::Fleet, "production"),
                ]
            );
            assert_eq!(
                snapshot.edges,
                vec![ResourceTopologyEdge {
                    kind: ResourceTopologyEdgeKind::FleetContainsProject,
                    from: resource(ResourceTopologyResourceKind::Fleet, "production"),
                    to: resource(ResourceTopologyResourceKind::Project, "site"),
                }]
            );
            assert_eq!(snapshot.diagnostics.len(), 3);
            assert!(snapshot.diagnostics.iter().any(|diagnostic| matches!(diagnostic,
                ResourceTopologyDiagnostic::UnresolvedReference { reference, edge_kind: ResourceTopologyEdgeKind::ProjectTargetsServer, .. }
                if reference == &resource(ResourceTopologyResourceKind::Server, "missing-server")
            )));
            assert!(snapshot.diagnostics.iter().any(|diagnostic| matches!(diagnostic,
                ResourceTopologyDiagnostic::UnresolvedReference { reference, edge_kind: ResourceTopologyEdgeKind::ProjectUsesComponent, .. }
                if reference == &resource(ResourceTopologyResourceKind::Component, "missing-component")
            )));
        });
    }

    #[test]
    fn resolves_runner_server_associations_without_owning_runner_configuration() {
        crate::test_support::with_isolated_home(|_| {
            server::save(&server::Server {
                id: "runner-host".to_string(),
                host: "runner.example.test".to_string(),
                user: "homeboy".to_string(),
                port: 22,
                aliases: Vec::new(),
                identity_file: None,
                kind: None,
                auth: None,
                env: Default::default(),
                runner: None,
            })
            .expect("server config");

            let snapshot = resolve(
                &[resource(ResourceTopologyResourceKind::Runner, "lab")],
                &[ResourceTopologyRunner {
                    id: "lab".to_string(),
                    server_id: Some("runner-host".to_string()),
                }],
            )
            .expect("topology");

            assert_eq!(
                snapshot.edges,
                vec![ResourceTopologyEdge {
                    kind: ResourceTopologyEdgeKind::RunnerUsesServer,
                    from: resource(ResourceTopologyResourceKind::Runner, "lab"),
                    to: resource(ResourceTopologyResourceKind::Server, "runner-host"),
                }]
            );
        });
    }
}
