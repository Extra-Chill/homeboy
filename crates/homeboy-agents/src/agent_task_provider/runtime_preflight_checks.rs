use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::agent_task::AgentTaskComponentContract;
use homeboy_core::{Error, Result};

use super::runtime_types::{
    AgentTaskRuntimePreflightCheck, AgentTaskRuntimePreflightCheckEnforcement,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimePreflightConflict {
    pub check: String,
    pub component: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct RuntimePreflightReadiness {
    pub ready: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<RuntimePreflightConflict>,
}

impl RuntimePreflightReadiness {
    fn ready() -> Self {
        Self {
            ready: true,
            conflicts: Vec::new(),
        }
    }
}

pub fn evaluate_runtime_preflight_checks(
    checks: &[AgentTaskRuntimePreflightCheck],
    component_contracts: &[AgentTaskComponentContract],
) -> RuntimePreflightReadiness {
    if checks.is_empty() {
        return RuntimePreflightReadiness::ready();
    }

    let mut conflicts = Vec::new();
    for check in checks {
        if check.path_probes.exists.is_empty() {
            continue;
        }
        for component in component_contracts {
            if !component_matches_check(component, check) {
                continue;
            }
            let Some(root) = component
                .path
                .as_deref()
                .filter(|path| !path.trim().is_empty())
            else {
                continue;
            };
            let root = PathBuf::from(root);
            let component_label = component_label(component, &root);
            for probe in &check.path_probes.exists {
                let probe_path = probe.path.trim().trim_matches('/');
                if probe_path.is_empty() {
                    continue;
                }
                let path = root.join(probe_path);
                if path.exists() {
                    conflicts.push(RuntimePreflightConflict {
                        check: check.id.clone(),
                        component: component_label.clone(),
                        path: probe_path.to_string(),
                        probe: empty_string_as_none(probe.id.clone()),
                        subject: empty_string_as_none(probe.subject.clone()),
                        owner: empty_string_as_none(probe.owner.clone()),
                        remediation: empty_string_as_none(probe.remediation.clone()),
                    });
                }
            }
        }
    }

    if conflicts.is_empty() {
        RuntimePreflightReadiness::ready()
    } else {
        RuntimePreflightReadiness {
            ready: false,
            conflicts,
        }
    }
}

pub fn ensure_runtime_preflight_checks(
    checks: &[AgentTaskRuntimePreflightCheck],
    component_contracts: &[AgentTaskComponentContract],
) -> Result<RuntimePreflightReadiness> {
    let readiness = evaluate_runtime_preflight_checks(checks, component_contracts);
    if readiness.ready {
        return Ok(readiness);
    }
    let enforced = checks
        .iter()
        .any(|check| check.enforcement_level() == AgentTaskRuntimePreflightCheckEnforcement::Error);
    if !enforced {
        return Ok(readiness);
    }
    Err(runtime_preflight_error(checks, &readiness.conflicts))
}

fn component_matches_check(
    component: &AgentTaskComponentContract,
    check: &AgentTaskRuntimePreflightCheck,
) -> bool {
    let selector = &check.target.component;
    for (key, expected) in &selector.metadata_equals {
        if component_metadata_value(component, key) != Some(expected) {
            return false;
        }
    }
    if !selector.metadata_any_equals.is_empty()
        && !selector
            .metadata_any_equals
            .iter()
            .any(|(key, expected)| component_metadata_value(component, key) == Some(expected))
    {
        return false;
    }
    true
}

fn component_metadata_value<'a>(
    component: &'a AgentTaskComponentContract,
    key: &str,
) -> Option<&'a Value> {
    component.extra.get(key)
}

fn component_label(component: &AgentTaskComponentContract, root: &std::path::Path) -> String {
    component
        .slug
        .clone()
        .or_else(|| {
            root.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| root.display().to_string())
}

fn runtime_preflight_error(
    checks: &[AgentTaskRuntimePreflightCheck],
    conflicts: &[RuntimePreflightConflict],
) -> Error {
    let summary = conflicts
        .iter()
        .map(|conflict| {
            let owner = conflict
                .owner
                .as_deref()
                .unwrap_or("declared runtime owner");
            let subject = conflict.subject.as_deref().unwrap_or("declared path");
            format!(
                "component `{}` contains `{}` (owned by `{}`) at `{}`",
                conflict.component, subject, owner, conflict.path
            )
        })
        .collect::<Vec<_>>()
        .join("; ");

    let mut hints = vec![
        "This is a declared runtime preflight failure; no task cells were queued.".to_string(),
    ];
    for conflict in conflicts {
        if let Some(remediation) = conflict.remediation.as_deref() {
            hints.push(remediation.to_string());
        }
    }
    for check in checks {
        if let Some(remediation) = check.remediation.as_deref() {
            hints.push(remediation.to_string());
        }
    }

    let details = serde_json::json!({ "conflicts": conflicts }).to_string();
    Error::validation_invalid_argument(
        "runtime_preflight_checks",
        format!(
            "Homeboy refused dispatch: declared runtime preflight found {} component path conflict(s): {summary}.",
            conflicts.len()
        ),
        Some(details),
        Some(hints),
    )
}

fn empty_string_as_none(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map};

    use super::*;

    fn check() -> AgentTaskRuntimePreflightCheck {
        serde_json::from_value(json!({
            "id": "runtime.package_shadow",
            "enforcement": "error",
            "target": {
                "component": {
                    "metadata_equals": { "loadMode": "runtime-loadable" },
                    "metadata_any_equals": { "activate": true }
                }
            },
            "path_probes": {
                "exists": [{
                    "id": "runtime-lib",
                    "path": "vendor/acme/runtime-lib",
                    "subject": "acme/runtime-lib",
                    "owner": "runtime-1",
                    "remediation": "Remove acme/runtime-lib from the component."
                }]
            },
            "remediation": "Use the runtime-owned copy."
        }))
        .expect("runtime check")
    }

    fn component(
        path: &std::path::Path,
        load_mode: &str,
        activate: bool,
    ) -> AgentTaskComponentContract {
        let mut extra = Map::new();
        extra.insert("loadMode".to_string(), json!(load_mode));
        extra.insert("activate".to_string(), json!(activate));
        AgentTaskComponentContract {
            slug: Some("provider-component".to_string()),
            path: Some(path.display().to_string()),
            extra,
        }
    }

    #[test]
    fn ready_when_declared_path_probe_is_absent() {
        let dir = tempfile::tempdir().expect("component dir");

        let readiness = ensure_runtime_preflight_checks(
            &[check()],
            &[component(dir.path(), "runtime-loadable", true)],
        )
        .expect("clean component passes");

        assert!(readiness.ready);
        assert!(readiness.conflicts.is_empty());
    }

    #[test]
    fn conflict_names_component_owner_subject_and_path() {
        let dir = tempfile::tempdir().expect("component dir");
        std::fs::create_dir_all(dir.path().join("vendor/acme/runtime-lib"))
            .expect("create conflict path");

        let err = ensure_runtime_preflight_checks(
            &[check()],
            &[component(dir.path(), "runtime-loadable", true)],
        )
        .expect_err("declared conflict is refused");

        assert_eq!(err.details["field"], "runtime_preflight_checks");
        assert!(err.message.contains("acme/runtime-lib"));
        assert!(err.message.contains("runtime-1"));
        assert!(err.message.contains("provider-component"));
    }

    #[test]
    fn unmatched_component_metadata_skips_probe() {
        let dir = tempfile::tempdir().expect("component dir");
        std::fs::create_dir_all(dir.path().join("vendor/acme/runtime-lib"))
            .expect("create conflict path");

        let readiness =
            ensure_runtime_preflight_checks(&[check()], &[component(dir.path(), "library", false)])
                .expect("unmatched component is out of scope");

        assert!(readiness.ready);
    }
}
