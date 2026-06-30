use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::RigSpec;

/// Declarative dependency materialization step for rigs and rig components.
///
/// The contract is intentionally tool-agnostic. Homeboy core owns identifiers,
/// paths, cache inputs, expected outputs, logs, artifacts, and safety metadata;
/// concrete package managers or products belong behind the referenced command
/// or provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DependencyMaterializationStepSpec {
    /// Stable step identifier within the rig.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,

    /// Generic input refs or values consumed by the step.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, serde_json::Value>,

    /// Command reference understood by the selected runner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Provider reference understood by an extension or runner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Environment variables passed to the materialization process.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    /// Working directory ref. Supports the same path expansion layer as other
    /// rig path strings when execution support is wired in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Component id whose effective path is the default working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,

    /// Outputs that prove the dependency tree was materialized.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_outputs: Vec<DependencyMaterializationOutputSpec>,

    /// Input refs that define the cache key for this step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_key_inputs: Vec<String>,

    /// Required mutation/risk classification for orchestration policy.
    #[serde(
        default,
        skip_serializing_if = "DependencyMaterializationSafety::is_unspecified"
    )]
    pub safety: DependencyMaterializationSafety,

    /// Artifacts captured from this step for later handoff/debugging.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<DependencyMaterializationArtifactSpec>,

    /// Log streams or files captured from this step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<DependencyMaterializationLogSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyMaterializationOutputSpec {
    /// Output path or ref expected after materialization.
    pub path: String,

    /// Expected output kind. Defaults to any path.
    #[serde(
        default,
        skip_serializing_if = "DependencyMaterializationOutputKind::is_path"
    )]
    pub kind: DependencyMaterializationOutputKind,

    /// Whether missing output fails the step.
    #[serde(default = "default_required")]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DependencyMaterializationOutputKind {
    #[default]
    Path,
    File,
    Dir,
}

impl DependencyMaterializationOutputKind {
    pub fn is_path(&self) -> bool {
        matches!(self, Self::Path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DependencyMaterializationSafety {
    #[default]
    Unspecified,
    ReadOnly,
    WritesWorkingTree,
    WritesCache,
    NetworkAccess,
    ExternalMutation,
}

impl DependencyMaterializationSafety {
    pub fn is_unspecified(&self) -> bool {
        matches!(self, Self::Unspecified)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyMaterializationArtifactSpec {
    /// Stable artifact identifier.
    pub id: String,

    /// Artifact path or ref produced by the step.
    pub path: String,

    /// Optional media/type label for downstream artifact consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyMaterializationLogSpec {
    /// Stable log identifier.
    pub id: String,

    /// Log path or stream ref.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedDependencyMaterializationStep {
    pub id: String,
    pub component: Option<String>,
    pub spec: DependencyMaterializationStepSpec,
}

fn default_required() -> bool {
    true
}

pub fn normalize_dependency_materialization_steps(
    rig: &RigSpec,
) -> Vec<NormalizedDependencyMaterializationStep> {
    let mut steps = Vec::new();

    for step in &rig.requirements.dependency_materialization {
        let spec = normalized_step_spec(step);
        let id = spec.id.clone();
        steps.push(NormalizedDependencyMaterializationStep {
            id,
            component: spec.component.clone(),
            spec,
        });
    }

    steps
}

pub fn validate_dependency_materialization_steps(rig: &RigSpec) -> Vec<String> {
    let mut errors = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for step in normalize_dependency_materialization_steps(rig) {
        let context = if step.id.is_empty() {
            "dependency_materialization step".to_string()
        } else {
            format!("dependency_materialization step '{}'", step.id)
        };

        if step.spec.id.is_empty() {
            errors.push(format!("{context} must declare id"));
        }
        if !step.id.is_empty() && !seen.insert(step.id.clone()) {
            errors.push(format!("{context} id is duplicated"));
        }

        match (&step.spec.command, &step.spec.provider) {
            (Some(_), Some(_)) => errors.push(format!(
                "{context} must declare exactly one of command or provider"
            )),
            (None, None) => errors.push(format!(
                "{context} must declare exactly one of command or provider"
            )),
            _ => {}
        }

        if let Some(component) = &step.spec.component {
            if !rig.components.contains_key(component) {
                errors.push(format!(
                    "{context} component ref '{component}' is not declared"
                ));
            }
        }

        if step.spec.safety.is_unspecified() {
            errors.push(format!("{context} must declare safety classification"));
        }

        for output in &step.spec.expected_outputs {
            if output.path.trim().is_empty() {
                errors.push(format!("{context} expected output path must not be empty"));
            }
        }

        for artifact in &step.spec.artifacts {
            if artifact.id.trim().is_empty() || artifact.path.trim().is_empty() {
                errors.push(format!("{context} artifact id and path must not be empty"));
            }
        }

        for log in &step.spec.logs {
            if log.id.trim().is_empty() || log.path.trim().is_empty() {
                errors.push(format!("{context} log id and path must not be empty"));
            }
        }
    }

    errors
}

fn normalized_step_spec(
    step: &DependencyMaterializationStepSpec,
) -> DependencyMaterializationStepSpec {
    let mut spec = step.clone();
    spec.id = spec.id.trim().to_string();
    spec.command = normalized_optional_ref(spec.command.as_deref());
    spec.provider = normalized_optional_ref(spec.provider.as_deref());
    spec.cwd = normalized_optional_ref(spec.cwd.as_deref());
    spec.component = spec
        .component
        .as_ref()
        .map(|value| value.trim().to_string());
    spec.cache_key_inputs = spec
        .cache_key_inputs
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    spec
}

fn normalized_optional_ref(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rig::spec::{ComponentSpec, RigSpec};
    use std::collections::HashMap;

    #[test]
    fn parses_generic_dependency_materialization_contract() {
        let rig: RigSpec = serde_json::from_str(
            r#"{
                "components": {
                    "app": { "path": "${env.DEV_ROOT}/app" }
                },
                "requirements": {
                    "dependency_materialization": [
                        {
                            "id": "prepare-cache",
                            "inputs": { "manifest": "${components.app.path}/dependency.json" },
                            "provider": "deps.generic.prepare",
                            "env": { "MODE": "ci" },
                            "cwd": "${components.app.path}",
                            "component": "app",
                            "expected_outputs": [
                                { "path": "${components.app.path}/.deps/ready", "kind": "file" }
                            ],
                            "cache_key_inputs": ["manifest", "runtime.lock"],
                            "safety": "writes_cache",
                            "artifacts": [
                                { "id": "summary", "path": "artifacts/deps-summary.json", "kind": "application/json" }
                            ],
                            "logs": [
                                { "id": "prepare", "path": "logs/deps.log" }
                            ]
                        }
                    ]
                }
            }"#,
        )
        .expect("parse dependency materialization contract");

        let steps = normalize_dependency_materialization_steps(&rig);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, "prepare-cache");
        assert_eq!(steps[0].component.as_deref(), Some("app"));
        assert_eq!(
            steps[0].spec.provider.as_deref(),
            Some("deps.generic.prepare")
        );
        assert_eq!(steps[0].spec.command, None);
        assert_eq!(
            steps[0].spec.safety,
            DependencyMaterializationSafety::WritesCache
        );
        assert_eq!(
            steps[0].spec.expected_outputs[0].kind,
            DependencyMaterializationOutputKind::File
        );
        assert_eq!(steps[0].spec.artifacts[0].id, "summary");
        assert!(validate_dependency_materialization_steps(&rig).is_empty());
    }

    #[test]
    fn normalizes_component_refs_on_dependency_materialization_steps() {
        let rig = RigSpec {
            components: HashMap::from([(
                "app".to_string(),
                ComponentSpec {
                    path: "packages/app".to_string(),
                    component_id: None,
                    path_setting: None,
                    checkout_root: None,
                    remote_url: None,
                    triage_remote_url: None,
                    stack: None,
                    branch: None,
                    r#ref: None,
                    default_ref: None,
                    extensions: None,
                },
            )]),
            requirements: crate::core::rig::spec::RigRequirementsSpec {
                dependency_materialization: vec![DependencyMaterializationStepSpec {
                    id: " prepare ".to_string(),
                    component: Some(" app ".to_string()),
                    command: Some(" deps.prepare ".to_string()),
                    safety: DependencyMaterializationSafety::WritesWorkingTree,
                    cache_key_inputs: vec![" lock ".to_string(), " ".to_string()],
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let steps = normalize_dependency_materialization_steps(&rig);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, "prepare");
        assert_eq!(steps[0].component.as_deref(), Some("app"));
        assert_eq!(steps[0].spec.component.as_deref(), Some("app"));
        assert_eq!(steps[0].spec.command.as_deref(), Some("deps.prepare"));
        assert_eq!(steps[0].spec.cache_key_inputs, vec!["lock".to_string()]);
        assert!(validate_dependency_materialization_steps(&rig).is_empty());
    }

    #[test]
    fn validates_executor_and_safety_classification() {
        let rig: RigSpec = serde_json::from_str(
            r#"{
                "requirements": {
                    "dependency_materialization": [
                        { "id": "missing-executor", "safety": "read_only" },
                        { "id": "ambiguous", "command": "deps.prepare", "provider": "deps.provider", "safety": "network_access" },
                        { "id": "missing-safety", "command": "deps.prepare" },
                        { "id": "bad-output", "provider": "deps.provider", "safety": "writes_working_tree", "expected_outputs": [{ "path": "" }] },
                        { "id": "bad-component", "provider": "deps.provider", "safety": "writes_cache", "component": "missing" }
                    ]
                }
            }"#,
        )
        .expect("parse invalid dependency materialization contract");

        let errors = validate_dependency_materialization_steps(&rig);
        assert!(errors.iter().any(
            |error| error.contains("missing-executor") && error.contains("command or provider")
        ));
        assert!(errors
            .iter()
            .any(|error| error.contains("ambiguous") && error.contains("command or provider")));
        assert!(errors.iter().any(
            |error| error.contains("missing-safety") && error.contains("safety classification")
        ));
        assert!(errors
            .iter()
            .any(|error| error.contains("bad-output") && error.contains("expected output path")));
        assert!(errors
            .iter()
            .any(|error| error.contains("bad-component") && error.contains("component ref")));
    }
}
