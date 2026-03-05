pub fn run(project_id: &str, config: &DeployConfig) -> Result<DeployOrchestrationResult> {
    let project = project::load(project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    deploy_components(config, &project, &ctx, &base_path)
}

/// Apply per-project component overrides to a cloned component.
///
/// If the project has `component_overrides` entries for this component,
/// merge them onto a clone. Only deploy-relevant fields are applied:
/// `extract_command`, `remote_owner`, `build_command`, `build_artifact`,
/// `deploy_strategy`, and `hooks`.
fn apply_component_overrides(component: &Component, project: &Project) -> Component {
    let overrides = match project.component_overrides.get(&component.id) {
        Some(v) if v.is_object() => v,
        _ => return component.clone(),
    };

    // Serialize the component to JSON, merge overrides, deserialize back.
    // This reuses serde for all field types without manual field-by-field code.
    let mut base = match serde_json::to_value(component) {
        Ok(v) => v,
        Err(_) => return component.clone(),
    };

    if let (Some(base_obj), Some(override_obj)) = (base.as_object_mut(), overrides.as_object()) {
        for (key, value) in override_obj {
            // Skip identity fields — overriding id/local_path/remote_path per-project
            // would break deploy targeting. Use project base_path for path changes.
            if matches!(
                key.as_str(),
                "id" | "local_path" | "remote_path" | "aliases"
            ) {
                continue;
            }
            base_obj.insert(key.clone(), value.clone());
        }
    }

    match serde_json::from_value::<Component>(base) {
        Ok(mut merged) => {
            // Preserve identity fields from original
            merged.id = component.id.clone();
            merged
        }
        Err(_) => component.clone(),
    }
}

/// Main deploy orchestration entry point.
/// Handles component selection, building, and deployment.
fn deploy_components(
    config: &DeployConfig,
    project: &Project,
    ctx: &RemoteProjectContext,
    base_path: &str,
) -> Result<DeployOrchestrationResult> {
    let loaded = load_project_components(&project.component_ids)?;
    if loaded.deployable.is_empty() {
        let message = if loaded.skipped.is_empty() {
            "No components configured for project".to_string()
        } else {
            format!(
                "No deployable components found — {} component(s) skipped (no build artifact or deploy strategy): {}",
                loaded.skipped.len(),
                loaded.skipped.join(", ")
            )
        };
        return Err(Error::validation_invalid_argument(
            "componentIds",
            message,
            None,
            Some(vec![
                "Ensure components have a buildArtifact, an extension with artifact_pattern, or deploy_strategy: \"git\"".to_string(),
                format!("Check with: homeboy component show <id>"),
            ]),
        ));
    }

    let components = plan_components(
        config,
        &loaded.deployable,
        &loaded.skipped,
        base_path,
        &ctx.client,
    )?;

    if components.is_empty() {
        return Ok(DeployOrchestrationResult {
            results: vec![],
            summary: DeploySummary {
                total: 0,
                succeeded: 0,
                failed: 0,
                skipped: 0,
            },
        });
    }

    // Gather versions
    let local_versions: HashMap<String, String> = components
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();
    let remote_versions = if config.outdated || config.dry_run || config.check {
        fetch_remote_versions(&components, base_path, &ctx.client)
    } else {
        HashMap::new()
    };

    // Check and dry-run modes return early without building or deploying
    if config.check {
        return Ok(run_check_mode(
            &components,
            &local_versions,
            &remote_versions,
            base_path,
        ));
    }
    if config.dry_run {
        return Ok(run_dry_run_mode(
            &components,
            &local_versions,
            &remote_versions,
            base_path,
            config,
        ));
    }

    // Sync: pull latest changes before deploying (unless --no-pull or --skip-build)
    if !config.no_pull && !config.skip_build {
        sync_components(&components)?;
    }

    if !config.force {
        check_uncommitted_changes(&components)?;
    }

    // Verify expected version if --version was specified
    if let Some(ref expected) = config.expected_version {
        verify_expected_version(&components, expected)?;
    }

    // Execute deployments
    let mut results: Vec<ComponentDeployResult> = vec![];
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;

    for component in &components {
        // Apply per-project overrides (e.g. different extract_command or remote_owner)
        let component = apply_component_overrides(component, project);
        let result = execute_component_deploy(
            &component,
            config,
            ctx,
            base_path,
            project,
            local_versions.get(&component.id).cloned(),
            remote_versions.get(&component.id).cloned(),
        );
        if result.status == "deployed" {
            succeeded += 1;
        } else {
            failed += 1;
        }
        results.push(result);
    }

    Ok(DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total: succeeded + failed,
            succeeded,
            failed,
            skipped: 0,
        },
    })
}

/// Check mode: return component status without building or deploying.
fn run_check_mode(
    components: &[Component],
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    base_path: &str,
) -> DeployOrchestrationResult {
    let results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let status = calculate_component_status(c, remote_versions);
            let release_state = calculate_release_state(c);
            let mut result = ComponentDeployResult::new(c, base_path)
                .with_status("checked")
                .with_versions(
                    local_versions.get(&c.id).cloned(),
                    remote_versions.get(&c.id).cloned(),
                )
                .with_component_status(status);
            if let Some(state) = release_state {
                result = result.with_release_state(state);
            }
            result
        })
        .collect();

    let total = results.len() as u32;
    DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total,
            succeeded: 0,
            failed: 0,
            skipped: 0,
        },
    }
}

/// Dry-run mode: return planned results without building or deploying.
fn run_dry_run_mode(
    components: &[Component],
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    base_path: &str,
    config: &DeployConfig,
) -> DeployOrchestrationResult {
    let results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let status = if config.check {
                calculate_component_status(c, remote_versions)
            } else {
                ComponentStatus::Unknown
            };
            let mut result = ComponentDeployResult::new(c, base_path)
                .with_status("planned")
                .with_versions(
                    local_versions.get(&c.id).cloned(),
                    remote_versions.get(&c.id).cloned(),
                );
            if config.check {
                result = result.with_component_status(status);
            }
            result
        })
        .collect();

    let total = results.len() as u32;
    DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total,
            succeeded: 0,
            failed: 0,
            skipped: 0,
        },
    }
}

/// Verify no components have uncommitted changes before deployment.
fn check_uncommitted_changes(components: &[Component]) -> Result<()> {
    let dirty: Vec<&str> = components
        .iter()
        .filter(|c| !git::is_workdir_clean(Path::new(&c.local_path)))
        .map(|c| c.id.as_str())
        .collect();

    if !dirty.is_empty() {
        return Err(Error::validation_invalid_argument(
            "components",
            format!("Components have uncommitted changes: {}", dirty.join(", ")),
            None,
            Some(vec![
                "Commit your changes before deploying to ensure deployed code is tracked"
                    .to_string(),
                "Use --force to deploy anyway".to_string(),
            ]),
        ));
    }
    Ok(())
}

/// Fetch and pull latest changes for each component before deploying.
///
/// Prevents deploying stale code when the local clone is behind remote.
/// Runs `git fetch` + `git pull` for each component that has an upstream.
/// Aborts if pull fails (e.g., merge conflicts).
fn sync_components(components: &[Component]) -> Result<()> {
    for component in components {
        let path = &component.local_path;

        // Check if behind remote
        match git::fetch_and_get_behind_count(path) {
            Ok(Some(behind)) => {
                log_status!(
                    "deploy",
                    "'{}' is {} commit(s) behind remote — pulling...",
                    component.id,
                    behind
                );
                let pull_result = git::pull(Some(&component.id))?;
                if !pull_result.success {
                    return Err(Error::git_command_failed(format!(
                        "Failed to pull '{}': {}",
                        component.id,
                        pull_result.stderr.lines().next().unwrap_or("unknown error")
                    )));
                }
                log_status!("deploy", "'{}' is now up to date", component.id);
            }
            Ok(None) => {
                // Not behind or no upstream — nothing to do
            }
            Err(_) => {
                // git fetch failed — warn but don't block (might be offline)
                log_status!(
                    "deploy",
                    "Warning: could not check remote status for '{}' — deploying local state",
                    component.id
                );
            }
        }
    }
    Ok(())
}

/// Verify that component versions match the expected version.
///
/// When `--version` is used, ensures the local version of each component
/// matches the asserted version. This catches cases where the local copy
/// has a different version than what was just released.
fn verify_expected_version(components: &[Component], expected: &str) -> Result<()> {
    let mut mismatches = Vec::new();

    for component in components {
        if let Some(local_version) = version::get_component_version(component) {
            if local_version != expected {
                mismatches.push(format!(
                    "'{}': local version is {} (expected {})",
                    component.id, local_version, expected
                ));
            }
        }
    }

    if !mismatches.is_empty() {
        return Err(Error::validation_invalid_argument(
            "version",
            format!("Version mismatch: {}", mismatches.join("; ")),
            None,
            Some(vec![
                "Pull latest changes: git pull".to_string(),
                "Or remove --version to deploy the current local version".to_string(),
            ]),
        ));
    }
    Ok(())
}
