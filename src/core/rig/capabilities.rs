//! Generic declarative rig requirements and capability checks.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::expand::expand_vars;
use super::pipeline::{PipelineOutcome, PipelineStepOutcome};
use super::spec::{ExecutableRequirementSpec, FilesystemAssertionSpec, RigSpec};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RigRequirementCheckPlan {
    pub kind: String,
    pub label: String,
    pub target: String,
}

pub fn plan_requirement_checks(rig: &RigSpec) -> Vec<RigRequirementCheckPlan> {
    rig.requirements
        .executables
        .iter()
        .map(|requirement| RigRequirementCheckPlan {
            kind: "executable".to_string(),
            label: executable_label(requirement),
            target: requirement.executable.clone(),
        })
        .chain(
            rig.requirements
                .filesystem_assertions
                .iter()
                .map(|assertion| RigRequirementCheckPlan {
                    kind: "filesystem".to_string(),
                    label: filesystem_label(assertion),
                    target: assertion.path.clone(),
                }),
        )
        .collect()
}

pub fn evaluate_requirements(rig: &RigSpec) -> PipelineOutcome {
    let mut steps = Vec::new();

    for requirement in &rig.requirements.executables {
        steps.push(outcome_for_result(
            "rig-requirement",
            executable_label(requirement),
            evaluate_executable(rig, requirement),
        ));
    }

    for assertion in &rig.requirements.filesystem_assertions {
        steps.push(outcome_for_result(
            "rig-requirement",
            filesystem_label(assertion),
            evaluate_filesystem_assertion(rig, assertion),
        ));
    }

    PipelineOutcome {
        name: "check".to_string(),
        passed: steps.iter().filter(|step| step.status == "pass").count(),
        failed: steps.iter().filter(|step| step.status == "fail").count(),
        steps,
    }
}

fn outcome_for_result(
    kind: &str,
    label: String,
    result: Result<(), String>,
) -> PipelineStepOutcome {
    match result {
        Ok(()) => PipelineStepOutcome {
            kind: kind.to_string(),
            label,
            status: "pass".to_string(),
            error: None,
        },
        Err(error) => PipelineStepOutcome {
            kind: kind.to_string(),
            label,
            status: "fail".to_string(),
            error: Some(error),
        },
    }
}

fn evaluate_executable(
    rig: &RigSpec,
    requirement: &ExecutableRequirementSpec,
) -> Result<(), String> {
    if requirement.executable.trim().is_empty() {
        return Err("executable requirement must declare a non-empty executable".to_string());
    }

    let declared = expand_vars(rig, &requirement.executable);
    let mut attempts = Vec::new();

    for name in executable_env_names(requirement) {
        match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => {
                let candidate = expand_vars(rig, &value);
                if find_executable(&candidate).is_some() {
                    return Ok(());
                }
                attempts.push(format!("{}={} (not executable)", name, candidate));
            }
            _ => attempts.push(format!("{} is unset or empty", name)),
        }
    }

    if find_executable(&declared).is_some() {
        return Ok(());
    }

    attempts.push(declared_executable_attempt(&declared));

    let mut message = format!(
        "executable `{}` could not be resolved; tried {}",
        declared,
        attempts.join(", ")
    );

    if let Some(remediation) = &requirement.remediation {
        message.push_str(&format!(". Remediation: {}", expand_vars(rig, remediation)));
    }

    Err(message)
}

fn executable_env_names(requirement: &ExecutableRequirementSpec) -> Vec<&str> {
    requirement
        .env
        .iter()
        .map(String::as_str)
        .chain(requirement.env_aliases.iter().map(String::as_str))
        .filter(|name| !name.trim().is_empty())
        .collect()
}

fn declared_executable_attempt(declared: &str) -> String {
    let path = Path::new(declared);
    if declared.contains(std::path::MAIN_SEPARATOR) || path.is_absolute() {
        format!("declared executable `{}`", declared)
    } else {
        format!("PATH lookup for `{}`", declared)
    }
}

fn evaluate_filesystem_assertion(
    rig: &RigSpec,
    assertion: &FilesystemAssertionSpec,
) -> Result<(), String> {
    if assertion.path.trim().is_empty() {
        return Err("filesystem assertion must declare a non-empty path".to_string());
    }

    let resolved = resolve_filesystem_assertion_path(rig, assertion);
    if assertion.kind.matches_path(&resolved) {
        return Ok(());
    }

    let mut message = format!(
        "{} assertion failed: {} (declared: {})",
        assertion.kind.label(),
        resolved.display(),
        assertion.path
    );
    if let Some(remediation) = &assertion.remediation {
        message.push_str(&format!(". Remediation: {}", expand_vars(rig, remediation)));
    }

    Err(message)
}

fn resolve_filesystem_assertion_path(
    rig: &RigSpec,
    assertion: &FilesystemAssertionSpec,
) -> PathBuf {
    let expanded = expand_vars(rig, &assertion.path);
    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        path
    } else if let Some(cwd) = &assertion.cwd {
        PathBuf::from(expand_vars(rig, cwd)).join(path)
    } else {
        path
    }
}

fn executable_label(requirement: &ExecutableRequirementSpec) -> String {
    requirement
        .label
        .clone()
        .unwrap_or_else(|| format!("require executable {}", requirement.executable))
}

fn filesystem_label(assertion: &FilesystemAssertionSpec) -> String {
    assertion
        .label
        .clone()
        .unwrap_or_else(|| format!("require {} {}", assertion.kind.label(), assertion.path))
}

fn find_executable(candidate: &str) -> Option<PathBuf> {
    let path = Path::new(candidate);
    if candidate.contains(std::path::MAIN_SEPARATOR) || path.is_absolute() {
        return is_executable_file(path).then(|| path.to_path_buf());
    }

    let path_var = std::env::var_os("PATH").map(OsString::from)?;

    std::env::split_paths(&path_var)
        .map(|dir| dir.join(candidate))
        .find(|path| is_executable_file(path))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::core::rig::spec::{FilesystemAssertionKind, RigRequirementsSpec, RigSpec};

    #[test]
    fn plans_executable_and_filesystem_requirements() {
        let rig = RigSpec {
            requirements: RigRequirementsSpec {
                executables: vec![ExecutableRequirementSpec {
                    executable: "sh".to_string(),
                    ..Default::default()
                }],
                filesystem_assertions: vec![FilesystemAssertionSpec {
                    path: "Cargo.toml".to_string(),
                    kind: FilesystemAssertionKind::File,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let plan = plan_requirement_checks(&rig);

        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].kind, "executable");
        assert_eq!(plan[1].kind, "filesystem");
    }

    #[test]
    fn evaluates_filesystem_assertions() {
        let temp = tempdir().expect("tempdir");
        let file = temp.path().join("artifact.json");
        fs::write(&file, "{}").expect("write file");
        let rig = RigSpec {
            requirements: RigRequirementsSpec {
                filesystem_assertions: vec![FilesystemAssertionSpec {
                    path: file.to_string_lossy().to_string(),
                    kind: FilesystemAssertionKind::File,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let outcome = evaluate_requirements(&rig);

        assert_eq!(outcome.passed, 1);
        assert_eq!(outcome.failed, 0);
    }

    #[test]
    fn reports_missing_executable() {
        let rig = RigSpec {
            requirements: RigRequirementsSpec {
                executables: vec![ExecutableRequirementSpec {
                    executable: "homeboy-definitely-missing-tool".to_string(),
                    remediation: Some("install it".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let outcome = evaluate_requirements(&rig);

        assert_eq!(outcome.failed, 1);
        assert!(outcome.steps[0]
            .error
            .as_deref()
            .unwrap()
            .contains("Remediation: install it"));
    }

    #[test]
    fn executable_requirement_uses_env_aliases_before_path() {
        let temp = tempdir().expect("tempdir");
        let alias_bin = temp.path().join("alias-tool");
        fs::write(&alias_bin, "#!/bin/sh\nexit 0\n").expect("write executable");
        make_executable(&alias_bin);

        let primary_env = "HOMEBOY_TEST_PRIMARY_TOOL_BIN";
        let alias_env = "HOMEBOY_TEST_ALIAS_TOOL_BIN";
        std::env::set_var(primary_env, temp.path().join("missing-tool"));
        std::env::set_var(alias_env, &alias_bin);

        let rig = RigSpec {
            requirements: RigRequirementsSpec {
                executables: vec![ExecutableRequirementSpec {
                    executable: "demo-tool".to_string(),
                    env: Some(primary_env.to_string()),
                    env_aliases: vec![alias_env.to_string()],
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let outcome = evaluate_requirements(&rig);

        std::env::remove_var(primary_env);
        std::env::remove_var(alias_env);
        assert_eq!(outcome.failed, 0, "outcome: {:?}", outcome.steps);
    }

    #[test]
    fn executable_requirement_reports_resolution_sources() {
        let primary_env = "HOMEBOY_TEST_MISSING_PRIMARY_TOOL_BIN";
        let alias_env = "HOMEBOY_TEST_EMPTY_ALIAS_TOOL_BIN";
        std::env::set_var(primary_env, "/nonexistent/primary-tool");
        std::env::set_var(alias_env, "");

        let rig = RigSpec {
            requirements: RigRequirementsSpec {
                executables: vec![ExecutableRequirementSpec {
                    executable: "/nonexistent/missing-tool".to_string(),
                    env: Some(primary_env.to_string()),
                    env_aliases: vec![alias_env.to_string()],
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let outcome = evaluate_requirements(&rig);

        std::env::remove_var(primary_env);
        std::env::remove_var(alias_env);
        assert_eq!(outcome.failed, 1);
        let error = outcome.steps[0].error.as_deref().unwrap_or_default();
        assert!(
            error.contains("HOMEBOY_TEST_MISSING_PRIMARY_TOOL_BIN=/nonexistent/primary-tool"),
            "error: {error}"
        );
        assert!(
            error.contains("HOMEBOY_TEST_EMPTY_ALIAS_TOOL_BIN is unset or empty"),
            "error: {error}"
        );
        assert!(
            error.contains("declared executable `/nonexistent/missing-tool`"),
            "error: {error}"
        );
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &std::path::Path) {}
}
