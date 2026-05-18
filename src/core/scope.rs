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
    fn scope_reports_kind_and_id() {
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
}
