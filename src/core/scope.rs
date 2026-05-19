//! Shared Homeboy scope model.
//!
//! A scope is the thing a command operates on. Some commands need a runtime
//! target, some need source code, and some aggregate multiple contexts. Keeping
//! this vocabulary explicit prevents every workflow from pretending to be
//! project-owned.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::core::component;
use crate::core::deploy::release_download::detect_remote_url;
use crate::core::error::{Error, Result};
use crate::core::{fleet, project, rig};

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

#[derive(Debug, Clone)]
pub struct ScopeComponentRef {
    pub component_id: String,
    pub local_path: String,
    pub remote_url: Option<String>,
    pub triage_remote_url: Option<String>,
    pub priority_labels: Option<Vec<String>>,
    pub sources: BTreeSet<String>,
    pub usage: BTreeSet<String>,
}

impl ScopeComponentRef {
    pub fn new(
        component_id: String,
        local_path: String,
        remote_url: Option<String>,
        triage_remote_url: Option<String>,
        source: String,
    ) -> Self {
        let mut sources = BTreeSet::new();
        sources.insert(source);
        Self {
            component_id,
            local_path,
            remote_url,
            triage_remote_url,
            priority_labels: None,
            sources,
            usage: BTreeSet::new(),
        }
    }

    pub fn with_priority_labels(mut self, priority_labels: Option<Vec<String>>) -> Self {
        self.priority_labels = priority_labels;
        self
    }
}

pub fn resolve_scope_components(scope: &Scope) -> Result<Vec<ScopeComponentRef>> {
    match scope {
        Scope::Component(component_id) => resolve_component_scope(component_id),
        Scope::Project(project_id) => resolve_project_scope(project_id),
        Scope::Fleet(fleet_id) => resolve_fleet_scope(fleet_id),
        Scope::Rig(rig_id) => resolve_rig_scope(rig_id),
        Scope::Workspace => resolve_workspace_scope(),
        Scope::Path { path, component_id } => resolve_path_scope(path, component_id.as_deref()),
    }
}

/// Resolve a scope to full component records for source-code commands.
///
/// Unlike `resolve_scope_components()`, this preserves command/runtime fields
/// such as extension settings, scripts, version targets, and deploy metadata.
/// Component-first commands should prefer this when they need to execute work
/// against the component rather than only report on it.
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

fn resolve_component_scope(component_id: &str) -> Result<Vec<ScopeComponentRef>> {
    let comp = component::load(component_id)?;
    let priority_labels = comp.priority_labels.clone();
    Ok(vec![ScopeComponentRef::new(
        comp.id,
        comp.local_path,
        comp.remote_url,
        comp.triage_remote_url,
        format!("component:{component_id}"),
    )
    .with_priority_labels(priority_labels)])
}

fn resolve_project_scope(project_id: &str) -> Result<Vec<ScopeComponentRef>> {
    let proj = project::load(project_id)?;
    Ok(proj
        .components
        .into_iter()
        .map(|attachment| {
            let comp = component::load(&attachment.id).ok();
            let remote_url = comp.as_ref().and_then(|c| c.remote_url.clone());
            let triage_remote_url = comp.as_ref().and_then(|c| c.triage_remote_url.clone());
            ScopeComponentRef::new(
                attachment.id.clone(),
                if attachment.local_path.is_empty() {
                    comp.as_ref()
                        .map(|c| c.local_path.clone())
                        .unwrap_or_default()
                } else {
                    attachment.local_path
                },
                remote_url,
                triage_remote_url,
                format!("project:{project_id}"),
            )
            .with_priority_labels(comp.and_then(|c| c.priority_labels))
        })
        .collect())
}

fn resolve_fleet_scope(fleet_id: &str) -> Result<Vec<ScopeComponentRef>> {
    let fl = fleet::load(fleet_id)?;
    let fleet_priority_labels = fl.priority_labels.clone();
    let mut refs = BTreeMap::new();

    for project_id in fl.project_ids {
        let project_refs = resolve_project_scope(&project_id)?;
        for mut component_ref in project_refs {
            if component_ref.priority_labels.is_none() {
                component_ref.priority_labels = fleet_priority_labels.clone();
            }
            component_ref.usage.insert(project_id.clone());
            merge_component_ref(&mut refs, component_ref);
        }
    }

    Ok(refs.into_values().collect())
}

fn resolve_rig_scope(rig_id: &str) -> Result<Vec<ScopeComponentRef>> {
    let spec = rig::load(rig_id)?;
    let mut refs = Vec::new();
    for (component_id, component_spec) in spec.components.iter() {
        let path = rig::expand::expand_vars(&spec, &component_spec.path);
        let mut component_ref = ScopeComponentRef::new(
            component_id.clone(),
            path,
            component_spec.remote_url.clone(),
            component_spec.triage_remote_url.clone(),
            format!("rig:{rig_id}"),
        );
        component_ref.usage.insert(rig_id.to_string());
        refs.push(component_ref);
    }
    refs.sort_by(|a, b| a.component_id.cmp(&b.component_id));
    Ok(refs)
}

fn resolve_fleet_component_records(fleet_id: &str) -> Result<Vec<component::Component>> {
    let fleet = fleet::load(fleet_id)?;
    let mut components = BTreeMap::new();

    for project_id in fleet.project_ids {
        for mut component in resolve_scope_component_records(&Scope::Project(project_id))? {
            if component.priority_labels.is_none() {
                component.priority_labels = fleet.priority_labels.clone();
            }
            components.entry(component.id.clone()).or_insert(component);
        }
    }

    Ok(components.into_values().collect())
}

fn resolve_rig_component_records(rig_id: &str) -> Result<Vec<component::Component>> {
    let spec = rig::load(rig_id)?;
    let mut components = Vec::new();

    for (component_id, component_spec) in spec.components.iter() {
        let local_path = rig::expand::expand_vars(&spec, &component_spec.path);
        let mut component = component::discover_from_portable(Path::new(&local_path))
            .or_else(|| component::load(component_id).ok())
            .unwrap_or_default();
        component.id = component_id.clone();
        component.local_path = local_path;
        if component.remote_url.is_none() {
            component.remote_url = component_spec.remote_url.clone();
        }
        if component.triage_remote_url.is_none() {
            component.triage_remote_url = component_spec.triage_remote_url.clone();
        }
        if component.extensions.is_none() {
            component.extensions = component_spec.extensions.clone();
        }
        components.push(component);
    }

    components.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(components)
}

fn resolve_path_component_record(
    path: &str,
    component_id: &Option<String>,
) -> Result<Vec<component::Component>> {
    let refs = resolve_path_scope(path, component_id.as_deref())?;
    let component_ref = refs.into_iter().next().ok_or_else(|| {
        Error::internal_unexpected("Path scope did not resolve a component".to_string())
    })?;
    let mut component = component::discover_from_portable(Path::new(&component_ref.local_path))
        .or_else(|| component::load(&component_ref.component_id).ok())
        .unwrap_or_default();
    if let Some(component_id) = component_id {
        component.id = component_id.clone();
    } else if component.id.is_empty() {
        component.id = component_ref.component_id;
    }
    component.local_path = component_ref.local_path;
    if component.remote_url.is_none() {
        component.remote_url = component_ref.remote_url;
    }
    if component.triage_remote_url.is_none() {
        component.triage_remote_url = component_ref.triage_remote_url;
    }
    Ok(vec![component])
}

fn resolve_workspace_component_records() -> Result<Vec<component::Component>> {
    let mut components = BTreeMap::new();

    for project in project::list()? {
        for component in resolve_scope_component_records(&Scope::Project(project.id))? {
            components.entry(component.id.clone()).or_insert(component);
        }
    }

    for rig in rig::list()? {
        for component in resolve_scope_component_records(&Scope::Rig(rig.id))? {
            components.entry(component.id.clone()).or_insert(component);
        }
    }

    for component in component::list()? {
        components.entry(component.id.clone()).or_insert(component);
    }

    Ok(components.into_values().collect())
}

fn resolve_path_scope(path: &str, component_id: Option<&str>) -> Result<Vec<ScopeComponentRef>> {
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

    let canonical = checkout
        .canonicalize()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| path.to_string());
    let synthesized_id = component_id
        .map(|id| id.to_string())
        .or_else(|| {
            checkout
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string())
        })
        .unwrap_or_else(|| "path".to_string());
    let source = format!("path:{canonical}");

    Ok(vec![ScopeComponentRef::new(
        synthesized_id,
        canonical.clone(),
        detect_remote_url(Path::new(&canonical)),
        None,
        source,
    )])
}

fn resolve_workspace_scope() -> Result<Vec<ScopeComponentRef>> {
    let mut refs = BTreeMap::new();

    for proj in project::list()? {
        for mut component_ref in resolve_project_scope(&proj.id)? {
            component_ref.usage.insert(proj.id.clone());
            merge_component_ref(&mut refs, component_ref);
        }
    }

    for spec in rig::list()? {
        for mut component_ref in resolve_rig_scope(&spec.id)? {
            component_ref.usage.insert(spec.id.clone());
            merge_component_ref(&mut refs, component_ref);
        }
    }

    for comp in component::list()? {
        let source = format!("component:{}", comp.id);
        let priority_labels = comp.priority_labels.clone();
        merge_component_ref(
            &mut refs,
            ScopeComponentRef::new(
                comp.id,
                comp.local_path,
                comp.remote_url,
                comp.triage_remote_url,
                source,
            )
            .with_priority_labels(priority_labels),
        );
    }

    Ok(refs.into_values().collect())
}

fn merge_component_ref(
    refs: &mut BTreeMap<String, ScopeComponentRef>,
    component_ref: ScopeComponentRef,
) {
    let entry = refs
        .entry(component_ref.component_id.clone())
        .or_insert_with(|| component_ref.clone());
    entry.sources.extend(component_ref.sources);
    entry.usage.extend(component_ref.usage);
    if entry.local_path.is_empty() && !component_ref.local_path.is_empty() {
        entry.local_path = component_ref.local_path;
    }
    if entry.remote_url.is_none() {
        entry.remote_url = component_ref.remote_url;
    }
    if entry.triage_remote_url.is_none() {
        entry.triage_remote_url = component_ref.triage_remote_url;
    }
    if entry.priority_labels.is_none() {
        entry.priority_labels = component_ref.priority_labels;
    }
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
            Scope::Rig("studio".to_string()).command_name("triage", ScopeKind::Component),
            "triage.rig"
        );
        assert_eq!(
            Scope::Path {
                path: "/tmp/checkout".to_string(),
                component_id: None,
            }
            .command_name("triage", ScopeKind::Component),
            "triage.component"
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

    #[test]
    fn test_resolve_scope_components() {
        let tmp = tempfile::TempDir::new().expect("checkout tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("git marker");

        let refs = resolve_scope_components(&Scope::Path {
            path: tmp.path().to_string_lossy().to_string(),
            component_id: Some("explicit-component".to_string()),
        })
        .expect("path scope refs");

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].component_id, "explicit-component");
        assert!(refs[0]
            .sources
            .iter()
            .any(|source| source.starts_with("path:")));
    }

    #[test]
    fn test_with_priority_labels() {
        let component_ref = ScopeComponentRef::new(
            "component".to_string(),
            "/tmp/component".to_string(),
            None,
            None,
            "component:component".to_string(),
        )
        .with_priority_labels(Some(vec!["urgent".to_string()]));

        assert_eq!(
            component_ref.priority_labels,
            Some(vec!["urgent".to_string()])
        );
    }
}
