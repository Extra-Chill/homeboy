fn execute_component_deploy(
    component: &Component,
    config: &DeployConfig,
    ctx: &RemoteProjectContext,
    base_path: &str,
    project: &Project,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> ComponentDeployResult {
    let is_git_deploy = component.deploy_strategy.as_deref() == Some("git");

    // Build (git-deploy and skip-build skip this step)
    let (build_exit_code, build_error) = if is_git_deploy || config.skip_build {
        (Some(0), None)
    } else if artifact_is_fresh(component) {
        log_status!(
            "deploy",
            "Artifact for '{}' is up-to-date, skipping build",
            component.id
        );
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

    // Resolve install directory
    let install_dir = match base_path::join_remote_path(Some(base_path), &component.remote_path) {
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

/// Deploy a component via artifact upload (rsync / extension override).
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
) -> ComponentDeployResult {
    // Resolve artifact path
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

    let artifact_path = match artifact::resolve_artifact_path(artifact_pattern) {
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
