//! Desktop launcher wrappers for rigs.
//!
//! v1 intentionally ships the smallest useful surface: a macOS `.app` bundle
//! whose executable is a shell script. Native Swift/Platypus launchers and
//! cross-platform `.desktop` / `.lnk` generators can layer on this contract.

use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use super::expand::expand_vars;
use super::spec::{AppLauncherPlatform, AppLauncherPreflight, RigSpec};
use crate::error::{Error, Result};

mod bundle;

const DEFAULT_MACOS_INSTALL_DIR: &str = "/Applications";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppLauncherAction {
    Install,
    Uninstall,
    Update,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppLauncherReport {
    pub rig_id: String,
    pub action: AppLauncherAction,
    pub platform: AppLauncherPlatform,
    pub launcher_path: String,
    pub target_app: String,
    pub dry_run: bool,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AppLauncherOptions {
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
struct ResolvedLauncher {
    platform: AppLauncherPlatform,
    display_name: String,
    bundle_id: String,
    target_path: String,
    launcher_path: PathBuf,
    preflight: Vec<AppLauncherPreflight>,
}

pub fn install(rig: &RigSpec, options: AppLauncherOptions) -> Result<AppLauncherReport> {
    install_inner(rig, options, true)
}

pub fn update(rig: &RigSpec, options: AppLauncherOptions) -> Result<AppLauncherReport> {
    let mut report = install_inner(rig, options, true)?;
    report.action = AppLauncherAction::Update;
    Ok(report)
}

pub fn uninstall(rig: &RigSpec, options: AppLauncherOptions) -> Result<AppLauncherReport> {
    uninstall_inner(rig, options, true)
}

fn uninstall_inner(
    rig: &RigSpec,
    options: AppLauncherOptions,
    enforce_platform: bool,
) -> Result<AppLauncherReport> {
    let launcher = resolve_launcher(rig)?;
    if enforce_platform && !options.dry_run {
        validate_platform(launcher.platform)?;
    }
    let files = bundle::planned_files(&launcher);

    if !options.dry_run && launcher.launcher_path.exists() {
        fs::remove_dir_all(&launcher.launcher_path).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to remove launcher {}: {}",
                launcher.launcher_path.display(),
                e
            ))
        })?;
    }

    Ok(report(
        rig,
        AppLauncherAction::Uninstall,
        &launcher,
        options.dry_run,
        files,
    ))
}

fn install_inner(
    rig: &RigSpec,
    options: AppLauncherOptions,
    enforce_platform: bool,
) -> Result<AppLauncherReport> {
    let launcher = resolve_launcher(rig)?;
    if enforce_platform && !options.dry_run {
        validate_platform(launcher.platform)?;
    }
    let files = bundle::planned_files(&launcher);

    if !options.dry_run {
        bundle::write_macos_bundle(rig, &launcher)?;
    }

    Ok(report(
        rig,
        AppLauncherAction::Install,
        &launcher,
        options.dry_run,
        files,
    ))
}

fn resolve_launcher(rig: &RigSpec) -> Result<ResolvedLauncher> {
    let spec = rig.app_launcher.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "app_launcher",
            format!("Rig '{}' does not declare an app_launcher block", rig.id),
            Some(rig.id.clone()),
            Some(vec![
                "Add app_launcher to the rig spec before running `homeboy rig app install`"
                    .to_string(),
            ]),
        )
    })?;

    if spec.wrapper_display_name.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "app_launcher.wrapper_display_name",
            "Wrapper display name cannot be empty",
            Some(rig.id.clone()),
            None,
        ));
    }
    if spec.wrapper_bundle_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "app_launcher.wrapper_bundle_id",
            "Wrapper bundle id cannot be empty",
            Some(rig.id.clone()),
            None,
        ));
    }

    let install_dir = spec
        .install_dir
        .as_deref()
        .map(|p| expand_vars(rig, p))
        .unwrap_or_else(|| DEFAULT_MACOS_INSTALL_DIR.to_string());
    let install_dir = PathBuf::from(install_dir);
    let display_name = spec.wrapper_display_name.trim().to_string();
    let launcher_path = install_dir.join(format!("{}.app", display_name));

    Ok(ResolvedLauncher {
        platform: spec.platform,
        display_name,
        bundle_id: spec.wrapper_bundle_id.trim().to_string(),
        target_path: expand_vars(rig, &spec.target_app),
        launcher_path,
        preflight: if spec.preflight.is_empty() {
            vec![AppLauncherPreflight::RigCheck]
        } else {
            spec.preflight.clone()
        },
    })
}

fn validate_platform(platform: AppLauncherPlatform) -> Result<()> {
    match platform {
        AppLauncherPlatform::Macos if cfg!(target_os = "macos") => Ok(()),
        AppLauncherPlatform::Macos => Err(Error::validation_invalid_argument(
            "app_launcher.platform",
            "macOS app launchers can only be installed on macOS; use --dry-run to preview generated paths",
            None,
            Some(vec![
                "Linux .desktop and Windows .lnk launchers are deferred from v1".to_string(),
            ]),
        )),
    }
}

fn report(
    rig: &RigSpec,
    action: AppLauncherAction,
    launcher: &ResolvedLauncher,
    dry_run: bool,
    files: Vec<String>,
) -> AppLauncherReport {
    AppLauncherReport {
        rig_id: rig.id.clone(),
        action,
        platform: launcher.platform,
        launcher_path: launcher.launcher_path.display().to_string(),
        target_app: launcher.target_path.clone(),
        dry_run,
        files,
    }
}

#[cfg(test)]
#[path = "../../../tests/core/rig/app_test.rs"]
mod app_test;
