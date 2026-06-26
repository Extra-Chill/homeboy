//! Requirement pipeline step — path / executable preconditions with optional
//! prepare command.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::super::expand::expand_vars;
use super::super::spec::{PipelineStep, RigSpec};
use super::command_step::run_command_step;
use super::component::resolve_component_path;
use crate::core::error::{Error, Result};

pub(super) fn run_requirement_step(
    rig: &RigSpec,
    pipeline_name: &str,
    step: &PipelineStep,
    settings: &[(String, String)],
) -> Result<()> {
    let PipelineStep::Requirement {
        path,
        file,
        dir,
        component,
        component_path_contains,
        executable,
        executable_env,
        executable_env_aliases,
        prepare_command,
        prepare_phases,
        cwd,
        env,
        remediation,
        ..
    } = step
    else {
        unreachable!("requirement helper only accepts requirement steps")
    };

    if component.is_some() != component_path_contains.is_some() {
        return Err(Error::validation_invalid_argument(
            "requirement.component_path_contains",
            "Requirement must specify both `component` and `component_path_contains` or neither",
            None,
            None,
        ));
    }

    if let (Some(component_id), Some(required)) = (component, component_path_contains) {
        let (_, component_path) = resolve_component_path(rig, component_id)?;
        if !component_path.contains(required) {
            return Err(requirement_failed(
                rig,
                format!(
                    "component `{}` path `{}` does not contain `{}`",
                    component_id, component_path, required
                ),
                remediation.as_deref(),
            ));
        }
    }

    let mut path_specs = Vec::new();
    if let Some(value) = path {
        path_specs.push(("path", value));
    }
    if let Some(value) = file {
        path_specs.push(("file", value));
    }
    if let Some(value) = dir {
        path_specs.push(("dir", value));
    }

    if path_specs.is_empty() && component_path_contains.is_none() && executable.is_none() {
        return Err(Error::validation_invalid_argument(
            "requirement",
            "Requirement must specify at least one of `path`, `file`, `dir`, `component_path_contains`, or `executable`",
            None,
            None,
        ));
    }

    let collect_missing = || {
        path_specs
            .iter()
            .filter_map(|(kind, declared)| {
                requirement_path_failure(rig, cwd.as_deref(), kind, declared)
            })
            .chain(requirement_executable_failure(
                rig,
                executable.as_deref(),
                executable_env.as_deref(),
                executable_env_aliases,
                env,
            ))
            .collect::<Vec<_>>()
    };

    let missing_before_prepare = collect_missing();

    if !missing_before_prepare.is_empty()
        && prepare_command.is_some()
        && prepare_phases.iter().any(|phase| phase == pipeline_name)
    {
        run_command_step(
            rig,
            prepare_command.as_deref().unwrap(),
            cwd.as_deref(),
            env,
            settings,
        )
        .map_err(|error| {
            requirement_failed(
                rig,
                format!(
                    "{}; prepare command failed: {}",
                    missing_before_prepare.join("; "),
                    error
                ),
                remediation.as_deref(),
            )
        })?;
    }

    let missing = collect_missing();

    if !missing.is_empty() {
        return Err(requirement_failed(
            rig,
            missing.join("; "),
            remediation.as_deref(),
        ));
    }

    Ok(())
}

fn requirement_path_failure(
    rig: &RigSpec,
    cwd: Option<&str>,
    kind: &str,
    declared: &str,
) -> Option<String> {
    let resolved = resolve_requirement_path(rig, cwd, declared);
    let ok = match kind {
        "file" => resolved.is_file(),
        "dir" => resolved.is_dir(),
        _ => resolved.exists(),
    };

    (!ok).then(|| {
        let display = resolved.display();
        if declared == display.to_string() {
            format!("{} does not exist: {}", kind, display)
        } else {
            format!(
                "{} does not exist: {} (declared: {})",
                kind, display, declared
            )
        }
    })
}

fn requirement_executable_failure(
    rig: &RigSpec,
    executable: Option<&str>,
    executable_env: Option<&str>,
    executable_env_aliases: &[String],
    env: &HashMap<String, String>,
) -> Option<String> {
    let executable = executable?;
    let executable = expand_vars(rig, executable);
    let mut attempts = Vec::new();

    for name in executable_env_names(executable_env, executable_env_aliases) {
        match env_value(name, env) {
            Some(value) if !value.trim().is_empty() => {
                let candidate = expand_vars(rig, &value);
                if find_executable(&candidate, env).is_some() {
                    return None;
                }
                attempts.push(format!("{}={} (not executable)", name, candidate));
            }
            _ => attempts.push(format!("{} is unset or empty", name)),
        }
    }

    if find_executable(&executable, env).is_some() {
        return None;
    }

    attempts.push(declared_executable_attempt(&executable));

    Some(format!(
        "executable `{}` could not be resolved; tried {}",
        executable,
        attempts.join(", ")
    ))
}

fn executable_env_names<'a>(
    executable_env: Option<&'a str>,
    executable_env_aliases: &'a [String],
) -> Vec<&'a str> {
    executable_env
        .into_iter()
        .chain(executable_env_aliases.iter().map(String::as_str))
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

fn env_value(name: &str, env: &HashMap<String, String>) -> Option<String> {
    env.get(name).cloned().or_else(|| std::env::var(name).ok())
}

fn find_executable(candidate: &str, env: &HashMap<String, String>) -> Option<PathBuf> {
    let path = Path::new(candidate);
    if candidate.contains(std::path::MAIN_SEPARATOR) || path.is_absolute() {
        return is_executable_file(path).then(|| path.to_path_buf());
    }

    let path_var = env
        .get("PATH")
        .map(OsString::from)
        .or_else(|| std::env::var_os("PATH"));
    let path_var = path_var?;

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

fn resolve_requirement_path(rig: &RigSpec, cwd: Option<&str>, declared: &str) -> PathBuf {
    let expanded = expand_vars(rig, declared);
    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        path
    } else if let Some(cwd) = cwd {
        PathBuf::from(expand_vars(rig, cwd)).join(path)
    } else {
        path
    }
}

fn requirement_failed(rig: &RigSpec, message: String, remediation: Option<&str>) -> Error {
    let detail = remediation
        .map(|text| format!("{}. Remediation: {}", message, expand_vars(rig, text)))
        .unwrap_or(message);
    Error::rig_pipeline_failed(&rig.id, "requirement", detail)
}
