use std::path::{Path, PathBuf};

use crate::build;
use crate::component::Component;
use crate::context::RemoteProjectContext;
use crate::error::Result;
use crate::extension::build::resolve_artifact_path_from_root;
use crate::git;
use crate::paths as base_path;
use crate::project::Project;

use super::planning::{calculate_directory_size, format_bytes};
use super::release_download;
use super::safety_and_artifact::{deploy_artifact, deploy_via_git, validate_deploy_target};
use super::types::{ComponentDeployResult, DeployConfig, DeployResult};
use super::version_overrides::{
    deploy_with_override, find_deploy_override, find_deploy_verification, is_self_deploy,
    prefer_installed_binary, run_post_deploy_hooks,
};

pub(super) fn execute_component_deploy(
    component: &Component,
    config: &DeployConfig,
    ctx: &RemoteProjectContext,
    base_path: &str,
    project: &Project,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> ComponentDeployResult {
    let is_git_deploy = component.deploy_strategy.as_deref() == Some("git");
    let is_file_deploy = component.deploy_strategy.as_deref() == Some("file");

    // Try downloading release artifact from GitHub instead of building locally.
    // This is the preferred path when the component has remote_url set.
    let release_artifact: Option<PathBuf> = if !is_git_deploy
        && !is_file_deploy
        && !config.skip_build
        && release_download::supports_release_deploy(component)
    {
        try_download_release_artifact(component)
    } else {
        None
    };

    // Build (git-deploy, file-deploy, skip-build, and release-download skip this step)
    let (build_exit_code, build_error) =
        if is_git_deploy || is_file_deploy || config.skip_build || release_artifact.is_some() {
            (Some(0), None)
        } else {
            build::build_component(component)
        };

    if let Some(ref error) = build_error {
        return ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            error.clone(),
        )
        .with_build_exit_code(build_exit_code);
    }

    // Auto-resolve remote_path for WordPress components when not explicitly set.
    // This is a deploy-time safety net; the primary resolution happens in
    // resolve_project_component (#812). Plugins deploy to wp-content/plugins/{id},
    // themes to wp-content/themes/{id}.
    let effective_remote_path = if component.remote_path.trim().is_empty() {
        if let Some(resolved) = component.auto_resolve_remote_path() {
            log_status!("deploy", "Auto-resolved remote path: {}", resolved);
            resolved
        } else {
            component.remote_path.clone()
        }
    } else {
        component.remote_path.clone()
    };

    // Warn when deploying to a WordPress path without remote_owner configured.
    // Files will end up as root:root which breaks WordPress functionality (#602).
    if component.remote_owner.is_none() && effective_remote_path.contains("wp-content/") {
        log_status!(
            "deploy",
            "⚠ Component '{}' deploys to a WordPress path but has no remote_owner set. \
             Files may deploy as root:root. \
             Fix: homeboy component set {} --json '{{\"remote_owner\":\"www-data:www-data\"}}'",
            component.id,
            component.id
        );
    }

    // Resolve install directory
    let install_dir = match base_path::join_remote_path(Some(base_path), &effective_remote_path) {
        Ok(v) => v,
        Err(err) => {
            return ComponentDeployResult::failed(
                component,
                base_path,
                local_version,
                remote_version,
                err.to_string(),
            )
            .with_build_exit_code(build_exit_code);
        }
    };

    // Safety check: prevent deploying to shared parent directories (issue #353)
    if let Err(err) = validate_deploy_target(&install_dir, base_path, &component.id) {
        return ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            err.to_string(),
        )
        .with_build_exit_code(build_exit_code);
    }

    // Dispatch by deploy strategy
    let strategy = component.deploy_strategy.as_deref().unwrap_or("rsync");

    if strategy == "git" {
        return execute_git_deploy(
            component,
            config,
            ctx,
            base_path,
            &install_dir,
            local_version,
            remote_version,
        );
    }

    if strategy == "file" {
        return execute_file_deploy(
            component,
            ctx,
            base_path,
            &install_dir,
            local_version,
            remote_version,
        );
    }

    execute_artifact_deploy(
        component,
        config,
        ctx,
        base_path,
        project,
        &install_dir,
        local_version,
        remote_version,
        build_exit_code,
        release_artifact.as_ref(),
    )
}

/// Deploy a component via git push strategy.
fn execute_git_deploy(
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
fn execute_file_deploy(
    component: &Component,
    ctx: &RemoteProjectContext,
    base_path: &str,
    install_dir: &str,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> ComponentDeployResult {
    let local_path = Path::new(&component.local_path);

    if !local_path.exists() {
        return ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            format!("Source file does not exist: {}", component.local_path),
        );
    }

    if !local_path.is_file() {
        return ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            format!(
                "Component '{}' has deploy_strategy 'file' but local_path is not a file: {}",
                component.id, component.local_path
            ),
        );
    }

    // Create the parent directory on the remote (not the file path itself!)
    let remote_parent = Path::new(install_dir)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(".");

    let mkdir_cmd = format!(
        "mkdir -p {}",
        crate::engine::shell::quote_path(remote_parent)
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

    let deploy_result = super::transfer::upload_file(&ctx.client, local_path, install_dir);

    match deploy_result {
        Ok(super::types::DeployResult {
            success: true,
            exit_code,
            ..
        }) => {
            // Fix ownership if configured
            if let Some(owner) = component.remote_owner.as_deref() {
                let chown_cmd = format!(
                    "chown {} {}",
                    crate::engine::shell::quote_arg(owner),
                    crate::engine::shell::quote_path(install_dir)
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

            super::version_overrides::run_post_deploy_hooks(
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
        Ok(super::types::DeployResult {
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

/// Deploy a component via artifact upload (rsync / extension override).
///
/// When `downloaded_artifact` is Some, it takes precedence over the local build artifact.
/// This is used when the artifact was downloaded from a GitHub release.
#[allow(clippy::too_many_arguments)]
fn execute_artifact_deploy(
    component: &Component,
    config: &DeployConfig,
    ctx: &RemoteProjectContext,
    base_path: &str,
    project: &Project,
    install_dir: &str,
    local_version: Option<String>,
    remote_version: Option<String>,
    build_exit_code: Option<i32>,
    downloaded_artifact: Option<&PathBuf>,
) -> ComponentDeployResult {
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
                return ComponentDeployResult::failed(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    format!(
                        "Component '{}' has no build_artifact configured",
                        component.id
                    ),
                )
                .with_build_exit_code(build_exit_code);
            }
        };

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
                return ComponentDeployResult::failed(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    error_msg,
                )
                .with_build_exit_code(build_exit_code);
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

    // Look up verification from extensions
    let verification = find_deploy_verification(install_dir);

    // Check for extension-defined deploy override
    let deploy_result =
        if let Some((override_config, extension)) = find_deploy_override(install_dir) {
            deploy_with_override(
                &ctx.client,
                &artifact_path,
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
                &artifact_path,
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
            ..
        }) => {
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
                .with_versions(local_version.clone(), local_version)
                .with_remote_path(install_dir.to_string())
                .with_build_exit_code(build_exit_code)
                .with_deploy_exit_code(Some(exit_code))
        }
        Ok(DeployResult {
            success: false,
            exit_code,
            error,
        }) => ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            error.unwrap_or_default(),
        )
        .with_remote_path(install_dir.to_string())
        .with_build_exit_code(build_exit_code)
        .with_deploy_exit_code(Some(exit_code)),
        Err(err) => ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            err.to_string(),
        )
        .with_remote_path(install_dir.to_string())
        .with_build_exit_code(build_exit_code),
    }
}

// =============================================================================
// Release Artifact Download
// =============================================================================

/// Try to download a release artifact from GitHub for the component's latest tag.
///
/// Returns `Some(path)` if successful, `None` if anything fails (falls back to local build).
fn try_download_release_artifact(component: &Component) -> Option<PathBuf> {
    let remote_url = component.remote_url.as_ref()?;
    let github = release_download::parse_github_url(remote_url)?;
    let artifact_name = release_download::resolve_artifact_name(component)?;

    // Get the latest tag from the local clone (already synced by the pipeline)
    let tag = git::get_latest_tag(&component.local_path).ok().flatten()?;

    log_status!(
        "deploy",
        "Attempting to download release artifact for '{}' tag {} from GitHub...",
        component.id,
        tag
    );

    match release_download::download_release_artifact(&github, &tag, &artifact_name) {
        Ok(path) => Some(path),
        Err(e) => {
            log_status!(
                "deploy",
                "Release download failed for '{}': {} — falling back to local build",
                component.id,
                e
            );
            None
        }
    }
}

// =============================================================================
// Cleanup Functions
// =============================================================================

/// Clean up build dependencies from component's local_path after successful deploy.
/// This is a best-effort operation - failures are logged but do not fail the deploy.
fn cleanup_build_dependencies(
    component: &Component,
    config: &DeployConfig,
) -> Result<Option<String>> {
    // Skip cleanup if disabled at component level
    if !component.auto_cleanup {
        return Ok(None);
    }

    // Skip cleanup if --keep-deps flag is set
    if config.keep_deps {
        return Ok(Some("skipped (--keep-deps flag)".to_string()));
    }

    // Collect cleanup paths from linked extensions
    let mut cleanup_paths = Vec::new();
    if let Some(ref extensions) = component.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = crate::extension::load_extension(extension_id) {
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
