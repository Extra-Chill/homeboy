use std::path::Path;

use crate::core::artifact_inputs;
use crate::core::component::Component;
use crate::core::context::RemoteProjectContext;
use crate::core::error::Result;
use crate::core::project::Project;

use super::super::effect::remote_version_after_deploy_effect;
use super::super::generated_artifacts::GeneratedBuildArtifactCleanupGuard;
use super::super::planning::{calculate_directory_size, format_bytes};
use super::super::safety_and_artifact::{deploy_artifact, deploy_via_git};
use super::super::types::{ComponentDeployResult, DeployConfig, DeployResult};
use super::super::version_overrides::{
    deploy_with_override, find_deploy_override, find_deploy_verification, is_self_deploy,
    run_post_deploy_hooks,
};
use super::prepare::PreparedComponentDeploy;

/// Deploy a component via git push strategy.
pub(super) fn execute_git_deploy(
    component: &Component,
    config: &DeployConfig,
    ctx: &RemoteProjectContext,
    base_path: &str,
    install_dir: &str,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> ComponentDeployResult {
    let git_config = component.git_deploy.clone().unwrap_or_default();
    let deploy_result = deploy_via_git(
        &ctx.client,
        install_dir,
        &git_config,
        local_version.as_deref(),
    );

    match deploy_result {
        Ok(DeployResult {
            success: true,
            exit_code,
            ..
        }) => {
            if let Ok(Some(summary)) = cleanup_build_dependencies(component, config) {
                log_status!("deploy", "Cleanup: {}", summary);
            }
            run_post_deploy_hooks(&ctx.client, component, install_dir, base_path);

            ComponentDeployResult::new(component, base_path)
                .with_status("deployed")
                .with_versions(local_version.clone(), local_version)
                .with_remote_path(install_dir.to_string())
                .with_deploy_exit_code(Some(exit_code))
        }
        Ok(DeployResult {
            error, exit_code, ..
        }) => ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            error.unwrap_or_default(),
        )
        .with_remote_path(install_dir.to_string())
        .with_deploy_exit_code(Some(exit_code)),
        Err(err) => ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            err.to_string(),
        )
        .with_remote_path(install_dir.to_string()),
    }
}

/// Deploy a single file component via atomic SCP.
///
/// File components (`deploy_strategy: "file"`) skip build entirely — the
/// `local_path` IS the artifact. The `remote_path` (resolved into `install_dir`)
/// is treated as the full destination file path, not a directory.
///
/// The parent directory is created on the remote if it doesn't exist.
/// Upload uses atomic SCP (temp file + mv) to prevent partial writes.
pub(super) fn execute_file_deploy(
    component: &Component,
    ctx: &RemoteProjectContext,
    base_path: &str,
    install_dir: &str,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> ComponentDeployResult {
    let local_path = Path::new(&component.local_path);

    if !local_path.exists() {
        let error = format!("Source file does not exist: {}", component.local_path);
        return failed_file_deploy_result(
            component,
            base_path,
            &local_version,
            &remote_version,
            error,
        );
    }

    if !local_path.is_file() {
        let error = format!(
            "Component '{}' has deploy_strategy 'file' but local_path is not a file: {}",
            component.id, component.local_path
        );
        return failed_file_deploy_result(
            component,
            base_path,
            &local_version,
            &remote_version,
            error,
        );
    }

    // Create the parent directory on the remote (not the file path itself!)
    let remote_parent = Path::new(install_dir)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(".");

    let mkdir_cmd = format!(
        "mkdir -p {}",
        crate::core::engine::shell::quote_path(remote_parent)
    );
    log_status!("deploy", "Ensuring remote directory: {}", remote_parent);
    let mkdir_output = ctx.client.execute(&mkdir_cmd);
    if !mkdir_output.success {
        return ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            format!("Failed to create remote directory: {}", mkdir_output.stderr),
        );
    }

    // Upload via atomic SCP (temp file + mv)
    log_status!(
        "deploy",
        "Deploying file: {} -> {}",
        local_path.display(),
        install_dir
    );

    let deploy_result = super::super::transfer::upload_file(&ctx.client, local_path, install_dir);

    match deploy_result {
        Ok(super::super::types::DeployResult {
            success: true,
            exit_code,
            ..
        }) => {
            // Fix ownership if configured
            if let Some(owner) = component.remote_owner.as_deref() {
                let chown_cmd = format!(
                    "chown {} {}",
                    crate::core::engine::shell::quote_arg(owner),
                    crate::core::engine::shell::quote_path(install_dir)
                );
                let chown_output = ctx.client.execute(&chown_cmd);
                if !chown_output.success {
                    log_status!(
                        "deploy",
                        "Warning: could not set ownership to {}: {}",
                        owner,
                        chown_output.stderr
                    );
                }
            }

            super::super::version_overrides::run_post_deploy_hooks(
                &ctx.client,
                component,
                install_dir,
                base_path,
            );

            ComponentDeployResult::new(component, base_path)
                .with_status("deployed")
                .with_versions(local_version.clone(), local_version)
                .with_remote_path(install_dir.to_string())
                .with_deploy_exit_code(Some(exit_code))
        }
        Ok(super::super::types::DeployResult {
            error, exit_code, ..
        }) => ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            error.unwrap_or_default(),
        )
        .with_remote_path(install_dir.to_string())
        .with_deploy_exit_code(Some(exit_code)),
        Err(err) => ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            err.to_string(),
        )
        .with_remote_path(install_dir.to_string()),
    }
}

fn failed_file_deploy_result(
    component: &Component,
    base_path: &str,
    local_version: &Option<String>,
    remote_version: &Option<String>,
    error: String,
) -> ComponentDeployResult {
    ComponentDeployResult::failed(
        component,
        base_path,
        local_version.clone(),
        remote_version.clone(),
        error,
    )
}

/// Deploy a component via artifact upload (rsync / extension override).
pub(super) fn execute_artifact_deploy(
    prepared: &PreparedComponentDeploy,
    ctx: &RemoteProjectContext,
    base_path: &str,
    project: &Project,
) -> ComponentDeployResult {
    let component = &prepared.component;
    let config = &prepared.config;
    let install_dir = prepared.install_dir.as_str();
    let _generated_cleanup_guard = GeneratedBuildArtifactCleanupGuard::new(
        Path::new(&component.local_path),
        prepared.cleanup_local_artifact,
    );
    let Some(artifact_path) = prepared.artifact_path.as_ref() else {
        return ComponentDeployResult::failed(
            component,
            base_path,
            prepared.local_version.clone(),
            prepared.remote_version.clone(),
            format!(
                "Component '{}' has no resolved build artifact",
                component.id
            ),
        )
        .with_build_exit_code(prepared.build_exit_code);
    };
    let artifact_input_metadata = match artifact_inputs::resolve_metadata(component) {
        Ok(inputs) => inputs,
        Err(err) => {
            return ComponentDeployResult::failed(
                component,
                base_path,
                prepared.local_version.clone(),
                prepared.remote_version.clone(),
                err.to_string(),
            )
            .with_remote_path(install_dir.to_string())
            .with_build_exit_code(prepared.build_exit_code);
        }
    };

    // Look up verification from extensions
    let verification = find_deploy_verification(install_dir);

    // Check for extension-defined deploy override
    let deploy_result =
        if let Some((override_config, extension)) = find_deploy_override(install_dir) {
            deploy_with_override(
                &ctx.client,
                artifact_path,
                install_dir,
                &override_config,
                &extension,
                verification.as_ref(),
                Some(base_path),
                project.domain.as_deref(),
                component.remote_owner.as_deref(),
                component.cli_path.as_deref(),
            )
        } else {
            deploy_artifact(
                &ctx.client,
                artifact_path,
                install_dir,
                component.extract_command.as_deref(),
                verification.as_ref(),
                component.remote_owner.as_deref(),
            )
        };

    match deploy_result {
        Ok(DeployResult {
            success: true,
            exit_code,
            effect,
            ..
        }) => {
            let reported_remote_version = match remote_version_after_deploy_effect(
                component,
                project,
                base_path,
                &ctx.client,
                effect.as_ref(),
                prepared.local_version.as_ref(),
            ) {
                Ok(version) => version,
                Err(error) => {
                    return ComponentDeployResult::failed(
                        component,
                        base_path,
                        prepared.local_version.clone(),
                        prepared.remote_version.clone(),
                        error,
                    )
                    .with_remote_path(install_dir.to_string())
                    .with_artifact_inputs(artifact_input_metadata)
                    .with_build_exit_code(prepared.build_exit_code)
                    .with_deploy_exit_code(Some(exit_code));
                }
            };

            if prepared.cleanup_local_artifact {
                cleanup_deploy_build_artifact(component, artifact_path);
            }

            if let Ok(Some(summary)) = cleanup_build_dependencies(component, config) {
                log_status!("deploy", "Cleanup: {}", summary);
            }
            if is_self_deploy(component) {
                log_status!(
                    "deploy",
                    "Deployed '{}' binary. Remote processes will use the new version on next invocation.",
                    component.id
                );
            }
            run_post_deploy_hooks(&ctx.client, component, install_dir, base_path);

            ComponentDeployResult::new(component, base_path)
                .with_status("deployed")
                .with_versions(prepared.local_version.clone(), reported_remote_version)
                .with_remote_path(install_dir.to_string())
                .with_artifact_inputs(artifact_input_metadata)
                .with_build_exit_code(prepared.build_exit_code)
                .with_deploy_exit_code(Some(exit_code))
        }
        Ok(DeployResult {
            success: false,
            exit_code,
            error,
            ..
        }) => ComponentDeployResult::failed(
            component,
            base_path,
            prepared.local_version.clone(),
            prepared.remote_version.clone(),
            error.unwrap_or_default(),
        )
        .with_remote_path(install_dir.to_string())
        .with_build_exit_code(prepared.build_exit_code)
        .with_deploy_exit_code(Some(exit_code)),
        Err(err) => ComponentDeployResult::failed(
            component,
            base_path,
            prepared.local_version.clone(),
            prepared.remote_version.clone(),
            err.to_string(),
        )
        .with_remote_path(install_dir.to_string())
        .with_build_exit_code(prepared.build_exit_code),
    }
}

/// Remove the local build artifact created for a deploy.
///
/// `homeboy build` intentionally leaves a package for humans to inspect. During
/// `homeboy deploy`, that same package is transient plumbing and should not
/// leave a clean checkout dirty after the upload succeeds.
pub(super) fn cleanup_deploy_build_artifact(component: &Component, artifact_path: &Path) {
    let local_path = Path::new(&component.local_path);
    if !artifact_path.starts_with(local_path) || !artifact_path.exists() || !artifact_path.is_file()
    {
        return;
    }

    let size_before = artifact_path.metadata().map(|m| m.len()).unwrap_or(0);
    match std::fs::remove_file(artifact_path) {
        Ok(()) => {
            log_status!(
                "cleanup",
                "Removed deploy artifact {} (freed {})",
                artifact_path.display(),
                format_bytes(size_before)
            );
            cleanup_empty_artifact_dirs(local_path, artifact_path.parent());
        }
        Err(err) => {
            log_status!(
                "cleanup",
                "Warning: failed to remove deploy artifact {}: {}",
                artifact_path.display(),
                err
            );
        }
    }
}

fn cleanup_empty_artifact_dirs(local_path: &Path, start_dir: Option<&Path>) {
    let Some(mut dir) = start_dir else {
        return;
    };

    while dir.starts_with(local_path) && dir != local_path {
        match std::fs::remove_dir(dir) {
            Ok(()) => {
                dir = match dir.parent() {
                    Some(parent) => parent,
                    None => break,
                };
            }
            Err(_) => break,
        }
    }
}

/// Clean up build dependencies from component's local_path after successful deploy.
/// This is a best-effort operation - failures are logged but do not fail the deploy.
fn cleanup_build_dependencies(
    component: &Component,
    config: &DeployConfig,
) -> Result<Option<String>> {
    if !component.auto_cleanup {
        return Ok(None);
    }

    if config.keep_deps {
        return Ok(Some("skipped (--keep-deps flag)".to_string()));
    }

    let mut cleanup_paths = Vec::new();
    if let Some(ref extensions) = component.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = crate::core::extension::load_extension(extension_id) {
                if let Some(ref build) = manifest.build {
                    cleanup_paths.extend(build.cleanup_paths.iter().cloned());
                }
            }
        }
    }

    if cleanup_paths.is_empty() {
        return Ok(Some(
            "skipped (no cleanup paths configured in extensions)".to_string(),
        ));
    }

    let local_path = Path::new(&component.local_path);
    let mut cleaned_paths = Vec::new();
    let mut total_bytes_freed = 0u64;

    for cleanup_path in &cleanup_paths {
        let full_path = local_path.join(cleanup_path);

        if !full_path.exists() {
            continue;
        }

        // Calculate size before deletion
        let size_before = if full_path.is_dir() {
            calculate_directory_size(&full_path).unwrap_or(0)
        } else {
            full_path.metadata().map(|m| m.len()).unwrap_or(0)
        };

        // Attempt to remove the path
        let cleanup_result = if full_path.is_dir() {
            std::fs::remove_dir_all(&full_path)
        } else {
            std::fs::remove_file(&full_path)
        };

        match cleanup_result {
            Ok(()) => {
                cleaned_paths.push(cleanup_path.clone());
                total_bytes_freed += size_before;
                log_status!(
                    "cleanup",
                    "Removed {} (freed {})",
                    cleanup_path,
                    format_bytes(size_before)
                );
            }
            Err(e) => {
                log_status!(
                    "cleanup",
                    "Warning: failed to remove {}: {}",
                    cleanup_path,
                    e
                );
                // Don't return error - cleanup is best-effort
            }
        }
    }

    if cleaned_paths.is_empty() {
        Ok(Some("no paths needed cleanup".to_string()))
    } else {
        let summary = format!(
            "cleaned {} path(s), freed {}",
            cleaned_paths.len(),
            format_bytes(total_bytes_freed)
        );
        Ok(Some(summary))
    }
}
