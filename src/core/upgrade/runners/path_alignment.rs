use super::super::helpers::current_version;
use super::super::helpers::version_is_newer;
use super::super::types::InstallMethod;
use super::*;
use crate::core::build_identity;
use crate::core::runner;
use crate::core::runner::Runner;
use crate::core::runner::RunnerExecOptions;
use crate::core::Result;
use regex::Regex;
use std::path::Path;

/// Applies a runner `homeboy_path` alignment that may update the configured path.
///
/// When the alignment carries an `update_to` target, the runner config is updated
/// and the in-flight upgrade state (`homeboy_path`, `new_version`, `path_drift`,
/// `path_update_detail`) is mutated to reflect the realignment; a failed update is
/// recorded as path drift instead.
#[allow(clippy::too_many_arguments)]
pub fn apply_runner_homeboy_path_alignment(
    runner_id: &str,
    alignment: RunnerHomeboyPathAlignment,
    original_homeboy_path: &str,
    bare_homeboy_version: Option<&str>,
    homeboy_path: &mut String,
    new_version: &mut Option<String>,
    path_drift: &mut Option<String>,
    path_update_detail: &mut Option<String>,
    update_homeboy_path: &mut impl FnMut(&str, &str) -> Result<()>,
) {
    let Some(new_path) = alignment.update_to.as_deref() else {
        return;
    };
    match update_homeboy_path(runner_id, new_path) {
        Ok(()) => {
            *homeboy_path = new_path.to_string();
            *new_version = bare_homeboy_version.map(str::to_string);
            *path_drift = None;
            *path_update_detail = Some(format!(
                "runner homeboy_path updated from `{}` to `{}` because bare `homeboy` reports {}",
                original_homeboy_path,
                new_path,
                bare_homeboy_version.unwrap_or("an upgraded version")
            ));
        }
        Err(err) => {
            *path_drift = Some(format!(
                "{}; automatic runner homeboy_path update failed: {}",
                alignment.drift.unwrap_or_else(|| {
                    format!(
                        "configured runner executable `{}` is stale",
                        original_homeboy_path
                    )
                }),
                err.message
            ));
        }
    }
}

pub struct SourceUpgradeHomeboyPathRealignment {
    pub homeboy_path: String,
    pub version: String,
    pub detail: String,
}

pub fn source_upgrade_homeboy_path_realignment(
    runner: &Runner,
    original_homeboy_path: &str,
    method_override: Option<InstallMethod>,
    command_source_path: Option<&str>,
    configured_homeboy_path: &str,
    configured_version: Option<&str>,
    expected_source_identity: Option<&str>,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> Option<SourceUpgradeHomeboyPathRealignment> {
    if method_override != Some(InstallMethod::Source) {
        return None;
    }
    let expected_identity = expected_source_identity
        .map(str::to_string)
        .unwrap_or_else(|| build_identity::current().display);
    let configured_identity = runner_homeboy_identity(runner, configured_homeboy_path, exec)
        .ok()
        .flatten();
    if configured_version == Some(current_version())
        && configured_identity.as_deref() == Some(expected_identity.as_str())
    {
        return None;
    }
    let source_path = command_source_path?.trim_end_matches('/');
    for build_dir in ["release", "debug"] {
        let candidate = format!("{source_path}/target/{build_dir}/homeboy");
        let Some(version) = runner_homeboy_version(runner, &candidate, exec)
            .ok()
            .flatten()
        else {
            continue;
        };
        if version != current_version() {
            continue;
        }
        let Some(identity) = runner_homeboy_identity(runner, &candidate, exec)
            .ok()
            .flatten()
        else {
            continue;
        };
        if identity != expected_identity {
            continue;
        }
        return Some(SourceUpgradeHomeboyPathRealignment {
            homeboy_path: candidate.clone(),
            version: version.clone(),
            detail: format!(
                "runner homeboy_path updated from `{original_homeboy_path}` to source-built `{candidate}` because it reports {identity}"
            ),
        });
    }

    None
}

pub struct RunnerHomeboyPathAlignment {
    pub drift: Option<String>,
    pub update_to: Option<String>,
}

pub struct FailedUpgradePathRecovery {
    pub homeboy_path: String,
    pub bare_version: Option<String>,
    pub detail: String,
}

pub struct StaleBareHomeboyRepair {
    pub bare_version: Option<String>,
    pub path_drift: Option<String>,
    pub detail: String,
}

pub fn repair_stale_bare_homeboy_after_upgrade(
    runner: &Runner,
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&str>,
    configured_version: &str,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> StaleBareHomeboyRepair {
    let repair_command = runner_upgrade_command("homeboy", force, method_override, source_path);
    let repair_result = exec(
        &runner.id,
        runner_exec_options(runner, repair_command.clone()),
    );
    let repair_detail = match repair_result {
        Ok((output, 0)) => runner_upgrade_detail(&output),
        Ok((output, exit_code)) => {
            let detail = runner_upgrade_detail(&output);
            let bare_version = runner_homeboy_version(runner, "homeboy", exec)
                .ok()
                .flatten();
            return StaleBareHomeboyRepair {
                path_drift: Some(format!(
                    "configured runner executable reports {configured_version}, but managed PATH-visible `homeboy` repair exited {exit_code}: {detail}"
                )),
                bare_version,
                detail: format!(
                    "managed PATH-visible `homeboy` repair failed with `{}`: {detail}",
                    repair_command
                        .iter()
                        .map(|arg| shell_arg(arg))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            };
        }
        Err(err) => {
            let bare_version = runner_homeboy_version(runner, "homeboy", exec)
                .ok()
                .flatten();
            return StaleBareHomeboyRepair {
                path_drift: Some(format!(
                    "configured runner executable reports {configured_version}, but managed PATH-visible `homeboy` repair failed: {}",
                    err.message
                )),
                bare_version,
                detail: format!(
                    "managed PATH-visible `homeboy` repair failed with `{}`: {}",
                    repair_command
                        .iter()
                        .map(|arg| shell_arg(arg))
                        .collect::<Vec<_>>()
                        .join(" "),
                    err.message
                ),
            };
        }
    };

    let bare_version = runner_homeboy_version(runner, "homeboy", exec)
        .ok()
        .flatten();
    if bare_version.as_deref() == Some(configured_version) {
        return StaleBareHomeboyRepair {
            bare_version,
            path_drift: None,
            detail: format!(
                "PATH-visible `homeboy` repaired with `{}` and now reports {configured_version}: {repair_detail}",
                repair_command
                    .iter()
                    .map(|arg| shell_arg(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        };
    }

    let bare_detail = bare_version
        .as_deref()
        .unwrap_or("an unknown version")
        .to_string();
    StaleBareHomeboyRepair {
        path_drift: Some(format!(
            "configured runner executable reports {configured_version}, but managed PATH-visible `homeboy` repair left bare `homeboy` at {bare_detail}"
        )),
        bare_version,
        detail: format!(
            "managed PATH-visible `homeboy` repair completed with `{}` but bare `homeboy` still reports {bare_detail}: {repair_detail}",
            repair_command
                .iter()
                .map(|arg| shell_arg(arg))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

pub fn recover_runner_homeboy_path_after_failed_upgrade(
    runner: &Runner,
    homeboy_path: &str,
    configured_version: Option<&str>,
    update_homeboy_path: &mut impl FnMut(&str, &str) -> Result<()>,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> std::result::Result<Option<FailedUpgradePathRecovery>, String> {
    if !is_auto_realignable_homeboy_path(homeboy_path) {
        return Ok(None);
    }

    let Some(configured_version) = configured_version else {
        return Ok(None);
    };
    let bare_version = runner_bare_homeboy_version(runner, homeboy_path, exec);
    let Some(alignment) = runner_homeboy_path_alignment(
        &runner.id,
        homeboy_path,
        Some(configured_version),
        bare_version.as_deref(),
    ) else {
        return Ok(None);
    };
    let Some(new_path) = alignment.update_to.as_deref() else {
        return Ok(None);
    };

    update_homeboy_path(&runner.id, new_path).map_err(|err| {
        format!(
            "{}; automatic runner homeboy_path update failed: {}",
            alignment.drift.unwrap_or_else(|| {
                format!("configured runner executable `{homeboy_path}` is stale")
            }),
            err.message
        )
    })?;

    let detail = format!(
        "runner homeboy_path updated from `{}` to `{}` after configured runner executable failed to upgrade because bare `homeboy` reports {}",
        homeboy_path,
        new_path,
        bare_version.as_deref().unwrap_or("an upgraded version")
    );

    Ok(Some(FailedUpgradePathRecovery {
        homeboy_path: new_path.to_string(),
        bare_version,
        detail,
    }))
}

pub fn runner_homeboy_path_alignment(
    runner_id: &str,
    homeboy_path: &str,
    configured_version: Option<&str>,
    bare_version: Option<&str>,
) -> Option<RunnerHomeboyPathAlignment> {
    if homeboy_path == "homeboy" {
        return None;
    }
    let configured_version = configured_version?;
    let bare_version = bare_version?;
    if bare_version == configured_version {
        return None;
    }

    let drift = format!(
        "configured runner executable `{}` reports {}, but bare `homeboy` reports {}",
        homeboy_path, configured_version, bare_version
    );

    if is_auto_realignable_homeboy_path(homeboy_path)
        && version_is_newer(bare_version, configured_version)
    {
        return Some(RunnerHomeboyPathAlignment {
            drift: Some(drift),
            update_to: Some("homeboy".to_string()),
        });
    }

    if version_is_newer(configured_version, bare_version) {
        return Some(RunnerHomeboyPathAlignment {
            drift: Some(format!(
                "{}; bare `homeboy` is older than the configured runner executable, so the runner remains degraded until PATH-visible `homeboy` is upgraded or the shadowing binary is removed. Inspect with `{}`",
                drift,
                runner_inspect_bare_homeboy_command(runner_id)
            )),
            update_to: None,
        });
    }

    Some(RunnerHomeboyPathAlignment {
        drift: Some(format!(
            "{}; automatic runner homeboy_path update is unsafe for this configured path. Remediate with `{}` after verifying bare `homeboy` is the intended runner binary",
            drift,
            runner_set_homeboy_path_command(runner_id, "homeboy")
        )),
        update_to: None,
    })
}

pub fn is_auto_realignable_homeboy_path(homeboy_path: &str) -> bool {
    is_versioned_homeboy_path(homeboy_path)
        || is_disposable_lab_workspace_homeboy_path(homeboy_path)
}

pub fn is_disposable_lab_workspace_homeboy_path(homeboy_path: &str) -> bool {
    if !homeboy_path.contains("/_lab_workspaces/") {
        return false;
    }
    let path = Path::new(homeboy_path);
    if path.file_name().and_then(|name| name.to_str()) != Some("homeboy") {
        return false;
    }

    matches!(
        path.parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str()),
        Some("debug" | "release")
    ) && path
        .parent()
        .and_then(|parent| parent.parent())
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some("target")
}

pub fn is_versioned_homeboy_path(homeboy_path: &str) -> bool {
    let Some(file_name) = Path::new(homeboy_path)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    Regex::new(r"^homeboy-\d+\.\d+\.\d+$")
        .map(|re| re.is_match(file_name))
        .unwrap_or(false)
}

pub fn update_runner_homeboy_path(runner_id: &str, homeboy_path: &str) -> Result<()> {
    let spec = serde_json::json!({ "homeboy_path": homeboy_path }).to_string();
    runner::merge(Some(runner_id), &spec, &[])?;
    Ok(())
}
