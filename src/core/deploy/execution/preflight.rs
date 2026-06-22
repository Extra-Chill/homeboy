use std::path::{Path, PathBuf};

use crate::core::component::Component;
use crate::core::engine::command;
use crate::core::extension::build::resolve_artifact_path_from_root;
use crate::core::release::version;

use super::super::types::{ComponentDeployResult, DeployConfig};
use super::super::version_overrides::{
    find_deploy_override, is_self_deploy, prefer_installed_binary,
};
use super::{bound_captured_read, ARTIFACT_VERSION_READ_LIMIT_BYTES};

fn failed_preflight_file_artifact_result(
    component: &Component,
    base_path: &str,
    build_exit_code: Option<i32>,
    local_version: Option<String>,
    remote_version: Option<String>,
    error: String,
) -> ComponentDeployResult {
    ComponentDeployResult::failed(component, base_path, local_version, remote_version, error)
        .with_build_exit_code(build_exit_code)
}

#[allow(clippy::result_large_err)]
pub(super) fn validate_preflight_file_artifact(
    component: &Component,
    base_path: &str,
    build_exit_code: Option<i32>,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> std::result::Result<(), ComponentDeployResult> {
    let local_path = Path::new(&component.local_path);

    if !local_path.exists() {
        return Err(failed_preflight_file_artifact_result(
            component,
            base_path,
            build_exit_code,
            local_version,
            remote_version,
            format!("Source file does not exist: {}", component.local_path),
        ));
    }

    if !local_path.is_file() {
        return Err(failed_preflight_file_artifact_result(
            component,
            base_path,
            build_exit_code,
            local_version,
            remote_version,
            format!(
                "Component '{}' has deploy_strategy 'file' but local_path is not a file: {}",
                component.id, component.local_path
            ),
        ));
    }

    Ok(())
}

#[allow(clippy::result_large_err)]
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_preflight_artifact_path(
    component: &Component,
    config: &DeployConfig,
    base_path: &str,
    install_dir: &str,
    local_version: Option<String>,
    remote_version: Option<String>,
    build_exit_code: Option<i32>,
    downloaded_artifact: Option<&PathBuf>,
) -> std::result::Result<PathBuf, ComponentDeployResult> {
    // Resolve artifact path — prefer downloaded release artifact over local build
    let artifact_path = if let Some(downloaded) = downloaded_artifact {
        log_status!(
            "deploy",
            "Using downloaded release artifact: {}",
            downloaded.display()
        );
        downloaded.clone()
    } else {
        let artifact_pattern = match component.build_artifact.as_ref() {
            Some(pattern) => pattern,
            None => {
                return Err(failed_preflight_artifact_result(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    build_exit_code,
                    format!(
                        "Component '{}' has no build_artifact configured",
                        component.id
                    ),
                ));
            }
        };

        if should_create_missing_archive_artifact(component, config, artifact_pattern) {
            if let Err(error) = create_archive_artifact_from_head(component, artifact_pattern) {
                return Err(failed_preflight_artifact_result(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    build_exit_code,
                    error,
                ));
            }
        }

        match resolve_artifact_path_from_root(
            artifact_pattern,
            Some(Path::new(&component.local_path)),
        ) {
            Ok(path) => path,
            Err(e) => {
                let error_msg = if config.skip_build {
                    format!("{}. Release build may have failed.", e)
                } else {
                    format!("{}. Run build first: homeboy build {}", e, component.id)
                };
                return Err(failed_preflight_artifact_result(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    build_exit_code,
                    error_msg,
                ));
            }
        }
    };

    // For self-deploy components (e.g. deploying homeboy itself), prefer the
    // installed binary over a stale build artifact. This handles the case where
    // `homeboy upgrade` installed a new binary but the build artifact is from a
    // previous version — without this, `deploy --shared` would push the old binary.
    let artifact_path = if is_self_deploy(component) {
        match prefer_installed_binary(&artifact_path) {
            Some(installed) => installed,
            None => artifact_path,
        }
    } else {
        artifact_path
    };

    if let Some(expected_version) = local_version.as_deref() {
        if let Err(error) =
            validate_predeploy_artifact_version(component, &artifact_path, expected_version)
        {
            return Err(failed_preflight_artifact_result(
                component,
                base_path,
                local_version,
                remote_version,
                build_exit_code,
                error,
            ));
        }
    }

    let has_deploy_override = find_deploy_override(install_dir).is_some();
    if artifact_requires_component_extract_command(
        &artifact_path,
        component.extract_command.is_some(),
        has_deploy_override,
    ) {
        return Err(failed_preflight_artifact_result(
            component,
            base_path,
            local_version,
            remote_version,
            build_exit_code,
            format!(
                "Archive artifact '{}' requires an extractCommand. \
                 Add one with: homeboy component set <id> --json '{{\"extract_command\": \"unzip -o {{{{artifact}}}} && rm {{{{artifact}}}}\"}}'",
                artifact_path.display()
            ),
        ));
    }

    Ok(artifact_path)
}

fn failed_preflight_artifact_result(
    component: &Component,
    base_path: &str,
    local_version: Option<String>,
    remote_version: Option<String>,
    build_exit_code: Option<i32>,
    error: String,
) -> ComponentDeployResult {
    ComponentDeployResult::failed(component, base_path, local_version, remote_version, error)
        .with_build_exit_code(build_exit_code)
}

fn should_create_missing_archive_artifact(
    component: &Component,
    config: &DeployConfig,
    artifact_pattern: &str,
) -> bool {
    !config.skip_build
        && is_literal_zip_artifact_pattern(artifact_pattern)
        && !resolved_literal_artifact_path(component, artifact_pattern).exists()
}

fn is_literal_zip_artifact_pattern(pattern: &str) -> bool {
    !pattern.contains('*')
        && !pattern.contains('?')
        && !pattern.contains('[')
        && !pattern.contains(']')
        && Path::new(pattern)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext == "zip")
}

fn resolved_literal_artifact_path(component: &Component, artifact_pattern: &str) -> PathBuf {
    let artifact_path = PathBuf::from(artifact_pattern);
    if artifact_path.is_absolute() {
        artifact_path
    } else {
        Path::new(&component.local_path).join(artifact_path)
    }
}

fn create_archive_artifact_from_head(
    component: &Component,
    artifact_pattern: &str,
) -> std::result::Result<(), String> {
    let artifact_path = resolved_literal_artifact_path(component, artifact_pattern);
    let parent = artifact_path.parent().ok_or_else(|| {
        format!(
            "Build artifact path must include a parent directory: {}",
            artifact_path.display()
        )
    })?;

    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create build artifact directory '{}': {}",
                parent.display(),
                error
            )
        })?;
    }

    let artifact_output = artifact_path.to_string_lossy().to_string();
    let prefix = format!("{}/", component.id);
    log_status!(
        "deploy",
        "Creating deploy archive artifact: {}",
        artifact_path.display()
    );

    command::run_in(
        &component.local_path,
        "git",
        &[
            "archive",
            "--format=zip",
            &format!("--prefix={prefix}"),
            &format!("--output={artifact_output}"),
            "HEAD",
        ],
        "create deploy archive artifact",
    )
    .map(|_| ())
    .map_err(|error| {
        format!(
            "Failed to create deploy archive artifact '{}'. The component build completed, but the configured build_artifact was missing. Ensure '{}' is a git checkout with a valid HEAD, or make scripts.build create the artifact explicitly. Error: {}",
            artifact_path.display(),
            component.id,
            error
        )
    })
}

pub(super) fn validate_predeploy_artifact_version(
    component: &Component,
    artifact_path: &Path,
    expected_version: &str,
) -> std::result::Result<(), String> {
    if artifact_path.extension().and_then(|ext| ext.to_str()) != Some("zip") {
        return Ok(());
    }

    let Some(targets) = component.version_targets.as_ref() else {
        return Ok(());
    };
    if targets.is_empty() {
        return Ok(());
    }

    let file = std::fs::File::open(artifact_path).map_err(|error| {
        format!(
            "Failed to inspect artifact '{}' before deploy: {}",
            artifact_path.display(),
            error
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        format!(
            "Failed to inspect artifact '{}' before deploy: {}",
            artifact_path.display(),
            error
        )
    })?;
    let entry_names: Vec<String> = (0..archive.len())
        .filter_map(|index| {
            archive
                .by_index(index)
                .ok()
                .map(|file| file.name().to_string())
        })
        .filter(|name| !name.ends_with('/'))
        .collect();

    for target in targets {
        let pattern = target
            .pattern
            .clone()
            .or_else(|| version::default_pattern_for_file(&target.file))
            .ok_or_else(|| {
                format!(
                    "Cannot inspect artifact '{}' before deploy: version target '{}' has no pattern",
                    artifact_path.display(),
                    target.file
                )
            })?
            .replace("\\\\", "\\");
        // The bump step writes `target.file` in the workspace (git-tracked source),
        // but the shipped artifact may carry the bumped version at a different path
        // (e.g. compiled `build/<block>/block.json` instead of source `blocks/...`).
        // When `artifact_path` is set, verify against that path inside the ZIP.
        let verify_file = target.artifact_path.as_deref().unwrap_or(&target.file);
        let Some(entry_name) = entry_names
            .iter()
            .find(|name| zip_entry_matches_version_target(name, verify_file))
        else {
            return Err(format!(
                "Artifact '{}' does not contain version target '{}' for component '{}'. Refusing to deploy unverified content.",
                artifact_path.display(),
                verify_file,
                component.id
            ));
        };

        let mut entry = archive.by_name(entry_name).map_err(|error| {
            format!(
                "Failed to read version target '{}' from artifact '{}': {}",
                entry_name,
                artifact_path.display(),
                error
            )
        })?;
        let (content, capture) = bound_captured_read(&mut entry, ARTIFACT_VERSION_READ_LIMIT_BYTES)
            .map_err(|error| {
                format!(
                    "Failed to read version target '{}' from artifact '{}': {}",
                    entry_name,
                    artifact_path.display(),
                    error
                )
            })?;
        if capture.truncated {
            log_status!(
                "deploy",
                "Version target '{}' in artifact '{}' is larger than the {}-byte read cap; \
                 inspecting the retained {} of {} bytes (trailing tail) for the version string.",
                entry_name,
                artifact_path.display(),
                capture.limit_bytes,
                capture.retained_bytes,
                capture.seen_bytes
            );
        }

        let observed = version::parse_version(&content, &pattern).ok_or_else(|| {
            format!(
                "Artifact '{}' contains version target '{}' but Homeboy could not parse a version with the configured pattern. Refusing to deploy unverified content.",
                artifact_path.display(),
                entry_name
            )
        })?;

        if observed != expected_version {
            return Err(format!(
                "Artifact '{}' contains version '{}' in '{}' but expected '{}'. Refusing to deploy mismatched release content.",
                artifact_path.display(),
                observed,
                entry_name,
                expected_version
            ));
        }
    }

    Ok(())
}

fn zip_entry_matches_version_target(entry_name: &str, target_file: &str) -> bool {
    if entry_name == target_file {
        return true;
    }

    let relative = entry_name
        .split_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or(entry_name);
    if relative == target_file {
        return true;
    }

    Path::new(target_file)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|basename| relative == basename)
}

fn artifact_requires_extract_command(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext, "zip" | "tar" | "gz" | "tgz"))
        .unwrap_or(false)
}

pub(super) fn artifact_requires_component_extract_command(
    path: &Path,
    has_component_extract_command: bool,
    has_deploy_override: bool,
) -> bool {
    artifact_requires_extract_command(path)
        && !has_component_extract_command
        && !has_deploy_override
}
