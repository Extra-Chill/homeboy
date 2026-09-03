//! Versioned, read-only resource topology contracts.
//!
//! These values describe stable resource references and declared relationships.
//! Configuration, lifecycle, readiness, and execution policy remain owned by
//! their existing subsystems.

use serde::{Deserialize, Serialize};

pub const RESOURCE_TOPOLOGY_SNAPSHOT_SCHEMA: &str = "homeboy/resource-topology-snapshot/v1";

/// A stable reference to a Homeboy resource. It carries no resource configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceTopologyResourceRef {
    pub kind: ResourceTopologyResourceKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ResourceTopologyResourceKind {
    Component,
    Project,
    Server,
    Fleet,
    Runner,
}

/// A declared directed relationship between two resource references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceTopologyEdge {
    pub kind: ResourceTopologyEdgeKind,
    pub from: ResourceTopologyResourceRef,
    pub to: ResourceTopologyResourceRef,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ResourceTopologyEdgeKind {
    FleetContainsProject,
    ProjectTargetsServer,
    ProjectUsesComponent,
    RunnerUsesServer,
}

/// A typed fact that prevents a declared relationship from resolving fully.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceTopologyDiagnostic {
    UnresolvedReference {
        reference: ResourceTopologyResourceRef,
        declared_by: ResourceTopologyResourceRef,
        edge_kind: ResourceTopologyEdgeKind,
    },
}

/// A rooted, read-only view of canonical resource identities and declared edges.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceTopologySnapshot {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceTopologyResourceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<ResourceTopologyEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<ResourceTopologyResourceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ResourceTopologyDiagnostic>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resource(kind: ResourceTopologyResourceKind, id: &str) -> ResourceTopologyResourceRef {
        ResourceTopologyResourceRef {
            kind,
            id: id.to_string(),
        }
    }

    #[test]
    fn snapshot_wire_shape_preserves_identity_edges_roots_and_unresolved_evidence() {
        let fleet = resource(ResourceTopologyResourceKind::Fleet, "production");
        let project = resource(ResourceTopologyResourceKind::Project, "site");
        let server = resource(ResourceTopologyResourceKind::Server, "primary");
        let component = resource(ResourceTopologyResourceKind::Component, "plugin");
        let runner = resource(ResourceTopologyResourceKind::Runner, "lab");
        let missing_component = resource(ResourceTopologyResourceKind::Component, "missing");
        let snapshot = ResourceTopologySnapshot {
            schema: RESOURCE_TOPOLOGY_SNAPSHOT_SCHEMA.to_string(),
            resources: vec![
                fleet.clone(),
                project.clone(),
                server.clone(),
                component.clone(),
                runner.clone(),
            ],
            edges: vec![
                ResourceTopologyEdge {
                    kind: ResourceTopologyEdgeKind::FleetContainsProject,
                    from: fleet.clone(),
                    to: project.clone(),
                },
                ResourceTopologyEdge {
                    kind: ResourceTopologyEdgeKind::ProjectTargetsServer,
                    from: project.clone(),
                    to: server.clone(),
                },
                ResourceTopologyEdge {
                    kind: ResourceTopologyEdgeKind::ProjectUsesComponent,
                    from: project.clone(),
                    to: component,
                },
                ResourceTopologyEdge {
                    kind: ResourceTopologyEdgeKind::RunnerUsesServer,
                    from: runner,
                    to: server,
                },
            ],
            roots: vec![fleet],
            diagnostics: vec![ResourceTopologyDiagnostic::UnresolvedReference {
                reference: missing_component,
                declared_by: project,
                edge_kind: ResourceTopologyEdgeKind::ProjectUsesComponent,
            }],
        };

        assert_eq!(
            serde_json::to_value(snapshot).expect("snapshot JSON"),
            json!({
                "schema": RESOURCE_TOPOLOGY_SNAPSHOT_SCHEMA,
                "resources": [
                    { "kind": "fleet", "id": "production" },
                    { "kind": "project", "id": "site" },
                    { "kind": "server", "id": "primary" },
                    { "kind": "component", "id": "plugin" },
                    { "kind": "runner", "id": "lab" }
                ],
                "edges": [
                    { "kind": "fleet_contains_project", "from": { "kind": "fleet", "id": "production" }, "to": { "kind": "project", "id": "site" } },
                    { "kind": "project_targets_server", "from": { "kind": "project", "id": "site" }, "to": { "kind": "server", "id": "primary" } },
                    { "kind": "project_uses_component", "from": { "kind": "project", "id": "site" }, "to": { "kind": "component", "id": "plugin" } },
                    { "kind": "runner_uses_server", "from": { "kind": "runner", "id": "lab" }, "to": { "kind": "server", "id": "primary" } }
                ],
                "roots": [{ "kind": "fleet", "id": "production" }],
                "diagnostics": [{
                    "kind": "unresolved_reference",
                    "reference": { "kind": "component", "id": "missing" },
                    "declared_by": { "kind": "project", "id": "site" },
                    "edge_kind": "project_uses_component"
                }]
            })
        );
    }

    #[test]
    fn snapshot_requires_an_explicit_versioned_schema() {
        let error = serde_json::from_value::<ResourceTopologySnapshot>(json!({}))
            .expect_err("schema is required");

        assert!(error.to_string().contains("missing field `schema`"));
    }
}
