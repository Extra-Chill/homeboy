use std::path::{Path, PathBuf};

use crate::core::build;
use crate::core::component::Component;
use crate::core::context::RemoteProjectContext;
use crate::core::engine::command;
use crate::core::error::Result;
use crate::core::extension::build::resolve_artifact_path_from_root;
use crate::core::git;
use crate::core::project::Project;

use super::path_roots::{component_remote_path, resolve_effective_remote_path};
use super::planning::{calculate_directory_size, format_bytes};
use super::policy::{owner_hint_for_path, protected_path_suffixes, validate_deploy_target};
use super::release_download;
use super::safety_and_artifact::{deploy_artifact, deploy_via_git};
use super::types::{ComponentDeployResult, DeployConfig, DeployResult};
use super::version_overrides::{
    deploy_with_override, find_deploy_override, find_deploy_verification, is_self_deploy,
    prefer_installed_binary, run_post_deploy_hooks,
};

pub(super) struct PreparedComponentDeploy {
    pub component: Component,
    pub config: DeployConfig,
    pub install_dir: String,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub build_exit_code: Option<i32>,
    pub artifact_path: Option<PathBuf>,
    pub cleanup_local_artifact: bool,
}

pub(super) fn prepare_component_deploy(
    component: &Component,
    config: &DeployConfig,
    base_path: &str,
    project: &Project,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> std::result::Result<PreparedComponentDeploy, ComponentDeployResult> {
    let is_git_deploy = component.deploy_strategy.as_deref() == Some("git");
    let is_file_deploy = component.deploy_strategy.as_deref() == Some("file");

    // Try downloading release artifact from GitHub instead of building locally.
    // This is the preferred path when the component has remote_url set.
    let release_artifact: Option<PathBuf> =
        if should_try_download_release_artifact(component, config, is_git_deploy, is_file_deploy) {
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
        return Err(ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            error.clone(),
        )
        .with_build_exit_code(build_exit_code));
    }

    // Auto-resolve remote_path from linked extension deploy policy when not explicitly set.
    // This is a deploy-time safety net; the primary resolution happens in
    // resolve_project_component (#812).
    let effective_remote_path = component_remote_path(component);
    if component.remote_path.trim().is_empty() && !effective_remote_path.trim().is_empty() {
        log_status!(
            "deploy",
            "Auto-resolved remote path: {}",
            effective_remote_path
        );
    }

    if component.remote_owner.is_none() {
        if let Some(suggested_owner) = owner_hint_for_path(component, &effective_remote_path) {
            log_status!(
                "deploy",
                "⚠ Component '{}' deploys to a path that may need remote_owner='{}'. \
             Files may deploy with the SSH user's ownership. \
             Fix: homeboy component set {} --json '{{\"remote_owner\":\"{}\"}}'",
                component.id,
                suggested_owner,
                component.id,
                suggested_owner
            );
        }
    }

    // Resolve and validate install directory before any destructive operation.
    let install_dir =
        match resolve_effective_remote_path(project, component, base_path).and_then(|install_dir| {
            validate_deploy_target(
                &install_dir,
                base_path,
                &component.id,
                &protected_path_suffixes(component),
            )?;
            Ok(install_dir)
        }) {
            Ok(install_dir) => install_dir,
            Err(err) => {
                return Err(failed_component_deploy_result(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    build_exit_code,
                    err.to_string(),
                ));
            }
        };

    let artifact_path = if is_git_deploy {
        None
    } else if is_file_deploy {
        validate_preflight_file_artifact(
            component,
            base_path,
            build_exit_code,
            local_version.clone(),
            remote_version.clone(),
        )?;
        None
    } else {
        match resolve_preflight_artifact_path(
            component,
            config,
            base_path,
            &install_dir,
            local_version.clone(),
            remote_version.clone(),
            build_exit_code,
            release_artifact.as_ref(),
        ) {
            Ok(path) => Some(path),
            Err(result) => return Err(result),
        }
    };

    let cleanup_local_artifact = artifact_path.as_ref().is_some_and(|_| {
        release_artifact.is_none() && !config.skip_build && !is_self_deploy(component)
    });

    Ok(PreparedComponentDeploy {
        component: component.clone(),
        config: config.clone(),
        install_dir,
        local_version,
        remote_version,
        build_exit_code,
        artifact_path,
        cleanup_local_artifact,
    })
}

pub(super) fn execute_preflighted_component_deploy(
    prepared: &PreparedComponentDeploy,
    ctx: &RemoteProjectContext,
    base_path: &str,
    project: &Project,
) -> ComponentDeployResult {
    let component = &prepared.component;

    // Dispatch by deploy strategy
    let strategy = component.deploy_strategy.as_deref().unwrap_or("rsync");

    if strategy == "git" {
        return execute_git_deploy(
            component,
            &prepared.config,
            ctx,
            base_path,
            &prepared.install_dir,
            prepared.local_version.clone(),
            prepared.remote_version.clone(),
        );
    }

    if strategy == "file" {
        return execute_file_deploy(
            component,
            ctx,
            base_path,
            &prepared.install_dir,
            prepared.local_version.clone(),
            prepared.remote_version.clone(),
        );
    }

    execute_artifact_deploy(prepared, ctx, base_path, project)
}

fn should_try_download_release_artifact(
    component: &Component,
    config: &DeployConfig,
    is_git_deploy: bool,
    is_file_deploy: bool,
) -> bool {
    !is_git_deploy
        && !is_file_deploy
        && !config.head
        && !config.tagged
        && !config.skip_build
        && release_download::supports_release_deploy(component)
        && !release_download::has_mutable_package_dependencies(component)
}

fn failed_component_deploy_result(
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

fn validate_preflight_file_artifact(
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

fn resolve_preflight_artifact_path(
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
                return Err(ComponentDeployResult::failed(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    format!(
                        "Component '{}' has no build_artifact configured",
                        component.id
                    ),
                )
                .with_build_exit_code(build_exit_code));
            }
        };

        if should_create_missing_archive_artifact(component, config, artifact_pattern) {
            if let Err(error) = create_archive_artifact_from_head(component, artifact_pattern) {
                return Err(ComponentDeployResult::failed(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    error,
                )
                .with_build_exit_code(build_exit_code));
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
                return Err(ComponentDeployResult::failed(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    error_msg,
                )
                .with_build_exit_code(build_exit_code));
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

    let has_deploy_override = find_deploy_override(install_dir).is_some();
    if artifact_requires_component_extract_command(
        &artifact_path,
        component.extract_command.is_some(),
        has_deploy_override,
    ) {
        return Err(ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            format!(
                "Archive artifact '{}' requires an extractCommand. \
                 Add one with: homeboy component set <id> --json '{{\"extract_command\": \"unzip -o {{artifact}} && rm {{artifact}}\"}}'",
                artifact_path.display()
            ),
        )
        .with_build_exit_code(build_exit_code));
    }

    Ok(artifact_path)
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

fn artifact_requires_extract_command(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext, "zip" | "tar" | "gz" | "tgz"))
        .unwrap_or(false)
}

fn artifact_requires_component_extract_command(
    path: &Path,
    has_component_extract_command: bool,
    has_deploy_override: bool,
) -> bool {
    artifact_requires_extract_command(path)
        && !has_component_extract_command
        && !has_deploy_override
}

/// Deploy a component via artifact upload (rsync / extension override).
fn execute_artifact_deploy(
    prepared: &PreparedComponentDeploy,
    ctx: &RemoteProjectContext,
    base_path: &str,
    project: &Project,
) -> ComponentDeployResult {
    let component = &prepared.component;
    let config = &prepared.config;
    let install_dir = prepared.install_dir.as_str();
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
            ..
        }) => {
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
                .with_versions(
                    prepared.local_version.clone(),
                    prepared.local_version.clone(),
                )
                .with_remote_path(install_dir.to_string())
                .with_build_exit_code(prepared.build_exit_code)
                .with_deploy_exit_code(Some(exit_code))
        }
        Ok(DeployResult {
            success: false,
            exit_code,
            error,
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
fn cleanup_deploy_build_artifact(component: &Component, artifact_path: &Path) {
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

#[cfg(test)]
mod tests {
    use super::{
        artifact_requires_component_extract_command, cleanup_deploy_build_artifact,
        failed_component_deploy_result, resolve_preflight_artifact_path,
        should_try_download_release_artifact,
    };
    use crate::core::component::Component;
    use crate::core::deploy::types::DeployConfig;
    use std::process::Command;

    #[test]
    fn test_execute_component_deploy_failure_helper_preserves_build_exit_code() {
        let component = Component {
            id: "example".to_string(),
            ..Component::default()
        };

        let result = failed_component_deploy_result(
            &component,
            "/srv/site",
            Some("1.0.0".to_string()),
            Some("0.9.0".to_string()),
            Some(7),
            "deploy failed".to_string(),
        );

        assert_eq!(result.id, "example");
        assert_eq!(result.status, "failed");
        assert_eq!(result.local_version.as_deref(), Some("1.0.0"));
        assert_eq!(result.remote_version.as_deref(), Some("0.9.0"));
        assert_eq!(result.build_exit_code, Some(7));
        assert_eq!(result.error.as_deref(), Some("deploy failed"));
    }

    #[test]
    fn archive_artifact_without_component_extract_is_allowed_by_deploy_override() {
        assert!(!artifact_requires_component_extract_command(
            std::path::Path::new("build/example.zip"),
            false,
            true,
        ));
    }

    #[test]
    fn archive_artifact_without_component_extract_or_override_requires_extract_command() {
        assert!(artifact_requires_component_extract_command(
            std::path::Path::new("build/example.zip"),
            false,
            false,
        ));
    }

    #[test]
    fn archive_artifact_with_component_extract_does_not_require_another_command() {
        assert!(!artifact_requires_component_extract_command(
            std::path::Path::new("build/example.zip"),
            true,
            false,
        ));
    }

    #[test]
    fn head_deploy_skips_release_artifact_download() {
        let component = Component {
            id: "example".to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: true,
            tagged: false,
        };

        assert!(!should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    #[test]
    fn tagged_deploy_skips_release_artifact_download() {
        let component = Component {
            id: "example".to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: false,
            tagged: true,
        };

        assert!(!should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    #[test]
    fn mutable_package_dependencies_skip_release_artifact_download() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("package.json"),
            r#"{
                "dependencies": {
                    "tokens": "github:Extra-Chill/extrachill-tokens#v0.7.2"
                }
            }"#,
        )
        .expect("write package.json");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: false,
            tagged: false,
        };

        assert!(!should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    #[test]
    fn registry_package_dependencies_allow_release_artifact_download() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("package.json"),
            r#"{
                "dependencies": {
                    "tokens": "^0.7.2"
                }
            }"#,
        )
        .expect("write package.json");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: false,
            tagged: false,
        };

        assert!(should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    #[test]
    fn cleanup_deploy_build_artifact_removes_zip_and_empty_build_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join("build");
        std::fs::create_dir_all(&build_dir).expect("mkdir build");
        let artifact = build_dir.join("example.zip");
        std::fs::write(&artifact, b"zip").expect("write artifact");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        cleanup_deploy_build_artifact(&component, &artifact);

        assert!(!artifact.exists());
        assert!(!build_dir.exists());
    }

    #[test]
    fn preflight_creates_missing_archive_artifact_from_tracked_head() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("plugin.php"), "<?php\n").expect("write plugin");
        std::fs::create_dir_all(temp.path().join("node_modules")).expect("mkdir node_modules");
        std::fs::write(temp.path().join("node_modules/junk.js"), "junk\n")
            .expect("write untracked dependency");
        git(temp.path(), &["init"]);
        git(temp.path(), &["add", "plugin.php"]);
        git(
            temp.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );

        let component = Component {
            id: "demo-plugin".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            build_artifact: Some("build/demo-plugin.zip".to_string()),
            extract_command: Some("unzip -o {{artifact}} && rm {{artifact}}".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: true,
            tagged: false,
        };

        let artifact = resolve_preflight_artifact_path(
            &component,
            &config,
            "/srv/site",
            "/srv/site/wp-content/plugins/demo-plugin",
            None,
            None,
            Some(0),
            None,
        )
        .expect("archive artifact should resolve");

        assert_eq!(artifact, temp.path().join("build/demo-plugin.zip"));
        let file = std::fs::File::open(&artifact).expect("open zip");
        let mut zip = zip::ZipArchive::new(file).expect("read zip");
        assert!(zip.by_name("demo-plugin/plugin.php").is_ok());
        assert!(zip.by_name("demo-plugin/node_modules/junk.js").is_err());
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn cleanup_deploy_build_artifact_preserves_non_empty_build_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join("build");
        std::fs::create_dir_all(&build_dir).expect("mkdir build");
        let artifact = build_dir.join("example.zip");
        let sibling = build_dir.join("keep.txt");
        std::fs::write(&artifact, b"zip").expect("write artifact");
        std::fs::write(&sibling, b"keep").expect("write sibling");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        cleanup_deploy_build_artifact(&component, &artifact);

        assert!(!artifact.exists());
        assert!(build_dir.exists());
        assert!(sibling.exists());
    }

    #[test]
    fn cleanup_deploy_build_artifact_ignores_paths_outside_component() {
        let component_dir = tempfile::tempdir().expect("component dir");
        let outside_dir = tempfile::tempdir().expect("outside dir");
        let artifact = outside_dir.path().join("example.zip");
        std::fs::write(&artifact, b"zip").expect("write artifact");
        let component = Component {
            id: "example".to_string(),
            local_path: component_dir.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        cleanup_deploy_build_artifact(&component, &artifact);

        assert!(artifact.exists());
    }
}
