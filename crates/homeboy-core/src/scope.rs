//! Shared Homeboy scope model.
//!
//! A scope is the thing a command operates on. Some commands need a runtime
//! target, some need source code, and some aggregate multiple contexts. Keeping
//! this vocabulary explicit prevents every workflow from pretending to be
//! project-owned.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

use crate::component;
use crate::error::{Error, Result};
use crate::git::release_download::detect_remote_url;
use crate::{fleet, project};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Component(String),
    Project(String),
    Fleet(String),
    Rig(String),
    Workspace,
    /// Operate on an unregistered checkout directly.
    Path {
        path: String,
        component_id: Option<String>,
    },
}

impl Scope {
    pub fn kind(&self) -> ScopeKind {
        match self {
            Scope::Component(_) => ScopeKind::Component,
            Scope::Project(_) => ScopeKind::Project,
            Scope::Fleet(_) => ScopeKind::Fleet,
            Scope::Rig(_) => ScopeKind::Rig,
            Scope::Workspace => ScopeKind::Workspace,
            Scope::Path { .. } => ScopeKind::Path,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        self.kind().as_str()
    }

    pub fn id(&self) -> &str {
        match self {
            Scope::Component(id) | Scope::Project(id) | Scope::Fleet(id) | Scope::Rig(id) => id,
            Scope::Workspace => "workspace",
            Scope::Path { path, component_id } => component_id.as_deref().unwrap_or(path.as_str()),
        }
    }

    pub fn command_name(&self, namespace: &str, path_kind: ScopeKind) -> String {
        let kind = if matches!(self, Scope::Path { .. }) {
            path_kind
        } else {
            self.kind()
        };
        format!("{namespace}.{}", kind.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Component,
    Project,
    Fleet,
    Rig,
    Workspace,
    Path,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::Component => "component",
            ScopeKind::Project => "project",
            ScopeKind::Fleet => "fleet",
            ScopeKind::Rig => "rig",
            ScopeKind::Workspace => "workspace",
            ScopeKind::Path => "path",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandScopeClass {
    Component,
    Target,
    Environment,
    Workspace,
}

impl CommandScopeClass {
    pub fn description(self) -> &'static str {
        match self {
            CommandScopeClass::Component => {
                "source-code commands: component IDs, paths, or CWD discovery"
            }
            CommandScopeClass::Target => {
                "runtime target commands: project/site/server context required"
            }
            CommandScopeClass::Environment => "local environment commands: rig or stack context",
            CommandScopeClass::Workspace => {
                "aggregate commands: workspace-wide filters and evidence"
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopeOutput {
    pub kind: &'static str,
    pub id: String,
}

impl From<&Scope> for ScopeOutput {
    fn from(scope: &Scope) -> Self {
        Self {
            kind: scope.kind_name(),
            id: scope.id().to_string(),
        }
    }
}

/// Resolve a scope to full component records for source-code commands.
///
/// Records preserve command/runtime fields such as extension settings,
/// scripts, version targets, and deploy metadata.
pub fn resolve_scope_component_records(scope: &Scope) -> Result<Vec<component::Component>> {
    match scope {
        Scope::Component(component_id) => Ok(vec![component::load(component_id)?]),
        Scope::Project(project_id) => {
            let project = project::load(project_id)?;
            project::resolve_project_components(&project)
        }
        Scope::Fleet(fleet_id) => resolve_fleet_component_records(fleet_id),
        Scope::Rig(rig_id) => resolve_rig_component_records(rig_id),
        Scope::Workspace => resolve_workspace_component_records(),
        Scope::Path { path, component_id } => resolve_path_component_record(path, component_id),
    }
}

fn resolve_fleet_component_records(fleet_id: &str) -> Result<Vec<component::Component>> {
    let fleet = fleet::load(fleet_id)?;
    let mut components = BTreeMap::new();

    for project_id in fleet.project_ids {
        for component in resolve_scope_component_records(&Scope::Project(project_id))? {
            components.entry(component.id.clone()).or_insert(component);
        }
    }

    Ok(components.into_values().collect())
}

fn resolve_rig_component_records(rig_id: &str) -> Result<Vec<component::Component>> {
    // Rig spec loading + component discovery is rig behavior, inverted
    // behind the rig provider hook.
    crate::rig_provider::resolve_rig_component_records(rig_id)
}

fn resolve_path_component_record(
    path: &str,
    component_id: &Option<String>,
) -> Result<Vec<component::Component>> {
    let checkout = Path::new(path);
    if !checkout.exists() {
        return Err(Error::validation_invalid_argument(
            "path",
            "Checkout path does not exist",
            Some(path.to_string()),
            None,
        ));
    }
    if !checkout.join(".git").exists() {
        return Err(Error::validation_invalid_argument(
            "path",
            "Checkout path is not a git repository (no `.git` entry)",
            Some(path.to_string()),
            None,
        ));
    }

    let local_path = checkout
        .canonicalize()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| path.to_string());
    let synthesized_id = component_id
        .clone()
        .or_else(|| {
            checkout
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string())
        })
        .unwrap_or_else(|| "path".to_string());

    let mut component = component::discover_from_portable(Path::new(&local_path))
        .or_else(|| component::load(&synthesized_id).ok())
        .unwrap_or_default();
    if let Some(component_id) = component_id {
        component.id = component_id.clone();
    } else if component.id.is_empty() {
        component.id = synthesized_id;
    }
    if component.remote_url.is_none() {
        component.remote_url = detect_remote_url(Path::new(&local_path));
    }
    component.local_path = local_path;
    Ok(vec![component])
}

fn resolve_workspace_component_records() -> Result<Vec<component::Component>> {
    let mut components = BTreeMap::new();

    for project in project::list()? {
        for component in resolve_scope_component_records(&Scope::Project(project.id))? {
            components.entry(component.id.clone()).or_insert(component);
        }
    }

    for rig_id in crate::rig_provider::rig_ids()? {
        for component in resolve_scope_component_records(&Scope::Rig(rig_id))? {
            components.entry(component.id.clone()).or_insert(component);
        }
    }

    for component in component::list()? {
        components.entry(component.id.clone()).or_insert(component);
    }

    Ok(components.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kind_name() {
        let scope = Scope::Rig("studio-combined".to_string());

        assert_eq!(scope.kind(), ScopeKind::Rig);
        assert_eq!(scope.kind_name(), "rig");
        assert_eq!(scope.id(), "studio-combined");
    }

    #[test]
    fn path_scope_uses_component_id_when_provided() {
        let scope = Scope::Path {
            path: "/tmp/checkout".to_string(),
            component_id: Some("homeboy".to_string()),
        };

        assert_eq!(scope.kind(), ScopeKind::Path);
        assert_eq!(scope.id(), "homeboy");
    }

    #[test]
    fn command_name_uses_scope_kind_with_path_override() {
        assert_eq!(
            Scope::Rig("studio".to_string()).command_name("status", ScopeKind::Component),
            "status.rig"
        );
        assert_eq!(
            Scope::Path {
                path: "/tmp/checkout".to_string(),
                component_id: None,
            }
            .command_name("status", ScopeKind::Component),
            "status.component"
        );
    }

    #[test]
    fn command_scope_class_descriptions_are_stable() {
        assert!(CommandScopeClass::Component
            .description()
            .contains("component IDs"));
        assert!(CommandScopeClass::Target
            .description()
            .contains("project/site/server"));
        assert!(CommandScopeClass::Environment
            .description()
            .contains("rig or stack"));
        assert!(CommandScopeClass::Workspace
            .description()
            .contains("workspace-wide"));
    }

    #[test]
    fn test_resolve_scope_component_records() {
        let tmp = tempfile::TempDir::new().expect("checkout tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("git marker");
        std::fs::write(
            tmp.path().join("homeboy.json"),
            serde_json::json!({
                "id": "portable-component",
                "extensions": { "rust": {} },
                "version_targets": [
                    { "file": "Cargo.toml", "pattern": "version = \\\"(.*)\\\"" }
                ]
            })
            .to_string(),
        )
        .expect("portable config");

        let records = resolve_scope_component_records(&Scope::Path {
            path: tmp.path().to_string_lossy().to_string(),
            component_id: None,
        })
        .expect("path scope records");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "portable-component");
        assert_eq!(
            records[0].local_path,
            tmp.path()
                .canonicalize()
                .expect("canonical checkout")
                .to_string_lossy()
        );
        assert!(records[0]
            .extensions
            .as_ref()
            .is_some_and(|extensions| extensions.contains_key("rust")));
        assert!(records[0].version_targets.is_some());
    }
}

#[cfg(test)]
mod wire_label_tests {
    use super::*;

    #[test]
    fn scope_kind_uses_fixed_wire_labels() {
        for (value, label) in [
            (ScopeKind::Component, "component"),
            (ScopeKind::Project, "project"),
            (ScopeKind::Fleet, "fleet"),
            (ScopeKind::Rig, "rig"),
            (ScopeKind::Workspace, "workspace"),
            (ScopeKind::Path, "path"),
        ] {
            assert_eq!(
                serde_json::to_value(&value).unwrap(),
                serde_json::json!(label)
            );
            assert_eq!(value.as_str(), label);
        }
    }
}
