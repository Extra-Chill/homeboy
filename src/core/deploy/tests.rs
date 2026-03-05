use super::*;

// =========================================================================
// validate_deploy_target — unit tests
//
// These validate the safety guard that prevents deploying to shared parent
// directories. All tests use generic paths — no framework-specific references.
// =========================================================================

#[test]
fn validate_accepts_component_subdirectory() {
    assert!(validate_deploy_target(
        "/srv/project/lib/my-component",
        "/srv/project",
        "my-component",
    )
    .is_ok());
}

#[test]
fn validate_accepts_deeply_nested_path() {
    assert!(
        validate_deploy_target("/srv/project/packages/core/src", "/srv/project", "core",).is_ok()
    );
}

#[test]
fn validate_accepts_arbitrary_safe_paths() {
    assert!(validate_deploy_target("/opt/apps/my-service", "/opt/apps", "my-service",).is_ok());
}

#[test]
fn validate_rejects_base_path() {
    let result = validate_deploy_target("/srv/project", "/srv/project", "my-component");
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("base_path"));
}

#[test]
fn validate_rejects_base_path_with_trailing_slash() {
    assert!(validate_deploy_target("/srv/project/", "/srv/project", "my-component",).is_err());
}

#[test]
fn validate_rejects_shared_vendor_directory() {
    let result = validate_deploy_target("/srv/project/vendor", "/srv/project", "my-lib");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .message
        .contains("shared parent directory"));
}

#[test]
fn validate_rejects_shared_node_modules_directory() {
    assert!(
        validate_deploy_target("/srv/project/node_modules", "/srv/project", "my-pkg",).is_err()
    );
}

#[test]
fn validate_rejects_shared_packages_directory() {
    assert!(validate_deploy_target("/srv/project/packages", "/srv/project", "my-pkg",).is_err());
}

#[test]
fn validate_rejects_shared_extensions_directory() {
    assert!(validate_deploy_target("/srv/project/extensions", "/srv/project", "my-ext",).is_err());
}

#[test]
fn validate_rejects_shared_plugins_directory() {
    assert!(
        validate_deploy_target("/srv/project/lib/plugins", "/srv/project", "my-plugin",).is_err()
    );
}

#[test]
fn validate_rejects_shared_themes_directory() {
    assert!(
        validate_deploy_target("/srv/project/lib/themes", "/srv/project", "my-theme",).is_err()
    );
}

#[test]
fn validate_rejects_trailing_slash_on_shared_dir() {
    assert!(validate_deploy_target("/srv/project/vendor/", "/srv/project", "my-lib",).is_err());
}

// =========================================================================
// Deploy safety integration tests — full path resolution chain (issue #353)
//
// These test the chain: base_path + remote_path → join_remote_path →
// validate_deploy_target, simulating the exact flow in execute_component_deploy.
// =========================================================================

/// Simulate the path resolution + validation chain used by execute_component_deploy.
fn resolve_and_validate(base_path: &str, remote_path: &str, component_id: &str) -> Result<String> {
    let install_dir = base_path::join_remote_path(Some(base_path), remote_path)?;
    validate_deploy_target(&install_dir, base_path, component_id)?;
    Ok(install_dir)
}

#[test]
fn chain_accepts_correct_component_path() {
    let result = resolve_and_validate("/srv/project", "lib/plugins/my-component", "my-component");
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "/srv/project/lib/plugins/my-component");
}

#[test]
fn chain_rejects_shared_parent_as_remote_path() {
    // The exact class of bug from issue #353: remote_path points to the
    // shared parent directory instead of the component's own subdirectory
    let result = resolve_and_validate("/srv/project", "lib/plugins", "my-component");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .message
        .contains("shared parent directory"));
}

#[test]
fn chain_rejects_trailing_slash_on_shared_parent() {
    let result = resolve_and_validate("/srv/project", "lib/plugins/", "my-component");
    assert!(result.is_err());
}

#[test]
fn chain_rejects_absolute_path_to_shared_parent() {
    let result = resolve_and_validate("/srv/project", "/srv/project/vendor", "my-lib");
    assert!(result.is_err());
}

#[test]
fn chain_rejects_base_path_as_remote_path() {
    let result = resolve_and_validate("/srv/project", "/srv/project", "my-component");
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("base_path"));
}

#[test]
fn chain_rejects_base_path_with_trailing_slash_mismatch() {
    let result = resolve_and_validate("/srv/project/", "vendor", "my-lib");
    assert!(result.is_err());
}

#[test]
fn chain_accepts_nested_component_directory() {
    let result = resolve_and_validate(
        "/srv/project",
        "lib/plugins/my-component/dist",
        "my-component",
    );
    assert!(result.is_ok());
}

#[test]
fn chain_accepts_flat_component_directory() {
    let result = resolve_and_validate("/opt/services", "my-service/current", "my-service");
    assert!(result.is_ok());
}

// =========================================================================
// Extension override template rendering — deploy safety
//
// Extension install commands use template variables to target directories.
// These tests verify that template rendering with correct vs incorrect
// variable values produces safe vs dangerous commands, documenting why
// upstream validation is essential.
// =========================================================================

#[test]
fn override_template_renders_safe_with_component_dir() {
    let template = "([ -d {{targetDir}} ] && rm -rf {{targetDir}} || true) && install {{artifact}}";

    let mut vars = HashMap::new();
    vars.insert(
        "targetDir".to_string(),
        "/srv/project/lib/plugins/my-component".to_string(),
    );
    vars.insert(
        "artifact".to_string(),
        "/tmp/staging/my-component.zip".to_string(),
    );

    let rendered = render_map(template, &vars);

    assert!(
        rendered.contains("rm -rf /srv/project/lib/plugins/my-component"),
        "rm -rf must target the component's own directory, got: {}",
        rendered
    );
}

#[test]
fn override_template_renders_dangerously_with_parent_dir() {
    // Documents the danger that validate_deploy_target prevents:
    // if targetDir is the shared parent, rm -rf destroys everything.
    let template = "rm -rf {{targetDir}}";

    let mut vars = HashMap::new();
    vars.insert(
        "targetDir".to_string(),
        "/srv/project/lib/plugins".to_string(),
    );

    let rendered = render_map(template, &vars);

    // This would be catastrophic — this is why validation must run first
    assert_eq!(rendered, "rm -rf /srv/project/lib/plugins");
}

// =========================================================================
// Clean command generation — pre-extraction cleanup safety
//
// deploy_artifact runs a find+rm command before extracting archives.
// These tests verify the command targets the right directory.
// =========================================================================

#[test]
fn clean_command_targets_component_directory() {
    let remote_path = "/srv/project/lib/plugins/my-component";
    let artifact_filename = "__homeboy_my-component.zip";

    let clean_cmd = format!(
        "cd {} && find . -mindepth 1 -maxdepth 1 ! -name {} -exec rm -rf {{}} +",
        shell::quote_path(remote_path),
        shell::quote_arg(artifact_filename),
    );

    assert!(
        clean_cmd.contains("cd '/srv/project/lib/plugins/my-component'"),
        "clean_cmd should cd into the component directory, got: {}",
        clean_cmd
    );
}

#[test]
fn clean_command_danger_with_shared_parent() {
    // Documents what the clean_cmd would look like if remote_path
    // pointed to the shared parent — this is what issue #353 was.
    let remote_path = "/srv/project/lib/plugins";
    let artifact_filename = "__homeboy_my-component.zip";

    let clean_cmd = format!(
        "cd {} && find . -mindepth 1 -maxdepth 1 ! -name {} -exec rm -rf {{}} +",
        shell::quote_path(remote_path),
        shell::quote_arg(artifact_filename),
    );

    // Would delete ALL sibling components — the exact bug from #353
    assert!(
        clean_cmd.contains("cd '/srv/project/lib/plugins'"),
        "Demonstrates the danger: {}",
        clean_cmd
    );
}

// =========================================================================
// DANGEROUS_PATH_SUFFIXES — exhaustive coverage
// =========================================================================

#[test]
fn all_dangerous_suffixes_are_rejected() {
    let base_path = "/srv/project";
    for suffix in DANGEROUS_PATH_SUFFIXES {
        let path = format!("/srv/project{}", suffix);
        let result = validate_deploy_target(&path, base_path, "test-component");
        assert!(
            result.is_err(),
            "Expected rejection for suffix '{}', path '{}'",
            suffix,
            path
        );
    }
}

#[test]
fn dangerous_suffix_with_component_subdirectory_is_safe() {
    let base_path = "/srv/project";
    for suffix in DANGEROUS_PATH_SUFFIXES {
        let path = format!("/srv/project{}/my-component", suffix);
        let result = validate_deploy_target(&path, base_path, "my-component");
        assert!(
            result.is_ok(),
            "Expected acceptance for path '{}' — has component subdirectory",
            path
        );
    }
}

// =========================================================================
// Error message quality — actionable remediation hints
// =========================================================================

#[test]
fn shared_parent_error_includes_component_id() {
    let err = validate_deploy_target("/srv/project/vendor", "/srv/project", "my-lib").unwrap_err();

    assert!(
        err.message.contains("my-lib"),
        "Error should mention the component ID for remediation: {}",
        err.message
    );
}

#[test]
fn shared_parent_error_suggests_correct_path() {
    let err = validate_deploy_target("/srv/project/vendor", "/srv/project", "my-lib").unwrap_err();

    assert!(
        err.message.contains("/srv/project/vendor/my-lib"),
        "Error should suggest the correct subdirectory path: {}",
        err.message
    );
}

#[test]
fn base_path_error_mentions_subdirectory() {
    let err = validate_deploy_target("/srv/project", "/srv/project", "my-component").unwrap_err();

    assert!(
        err.message.contains("subdirectory"),
        "Error should guide toward using a subdirectory: {}",
        err.message
    );
}

// =========================================================================
// Per-project component overrides (issue #386)
// =========================================================================

#[test]
fn apply_overrides_replaces_extract_command() {
    let component = Component::new(
        "data-machine".to_string(),
        "/local/path".to_string(),
        "wp-content/plugins/data-machine".to_string(),
        Some("dist.zip".to_string()),
    );
    let mut project = Project::default();
    project.component_overrides.insert(
        "data-machine".to_string(),
        serde_json::json!({
            "extract_command": "unzip -o {artifact} && chown -R opencode:opencode data-machine"
        }),
    );

    let result = apply_component_overrides(&component, &project);
    assert_eq!(
        result.extract_command.as_deref(),
        Some("unzip -o {artifact} && chown -R opencode:opencode data-machine")
    );
    // Identity preserved
    assert_eq!(result.id, "data-machine");
    assert_eq!(result.local_path, "/local/path");
    assert_eq!(result.remote_path, "wp-content/plugins/data-machine");
}

#[test]
fn apply_overrides_replaces_remote_owner() {
    let mut component = Component::new(
        "data-machine".to_string(),
        "/local/path".to_string(),
        "wp-content/plugins/data-machine".to_string(),
        Some("dist.zip".to_string()),
    );
    component.remote_owner = Some("www-data:www-data".to_string());

    let mut project = Project::default();
    project.component_overrides.insert(
        "data-machine".to_string(),
        serde_json::json!({
            "remote_owner": "opencode:opencode"
        }),
    );

    let result = apply_component_overrides(&component, &project);
    assert_eq!(result.remote_owner.as_deref(), Some("opencode:opencode"));
}

#[test]
fn apply_overrides_no_override_returns_clone() {
    let component = Component::new(
        "data-machine".to_string(),
        "/local/path".to_string(),
        "wp-content/plugins/data-machine".to_string(),
        Some("dist.zip".to_string()),
    );
    let project = Project::default();

    let result = apply_component_overrides(&component, &project);
    assert_eq!(result.id, component.id);
    assert_eq!(result.extract_command, component.extract_command);
}

#[test]
fn apply_overrides_skips_identity_fields() {
    let component = Component::new(
        "data-machine".to_string(),
        "/local/path".to_string(),
        "wp-content/plugins/data-machine".to_string(),
        Some("dist.zip".to_string()),
    );
    let mut project = Project::default();
    project.component_overrides.insert(
        "data-machine".to_string(),
        serde_json::json!({
            "id": "evil-override",
            "local_path": "/evil/path",
            "remote_path": "/evil/remote",
            "extract_command": "unzip {artifact}"
        }),
    );

    let result = apply_component_overrides(&component, &project);
    // Identity fields are protected
    assert_eq!(result.id, "data-machine");
    assert_eq!(result.local_path, "/local/path");
    assert_eq!(result.remote_path, "wp-content/plugins/data-machine");
    // Non-identity fields are applied
    assert_eq!(result.extract_command.as_deref(), Some("unzip {artifact}"));
}

#[test]
fn apply_overrides_wrong_component_id_is_noop() {
    let component = Component::new(
        "data-machine".to_string(),
        "/local/path".to_string(),
        "wp-content/plugins/data-machine".to_string(),
        Some("dist.zip".to_string()),
    );
    let mut project = Project::default();
    project.component_overrides.insert(
        "other-component".to_string(),
        serde_json::json!({
            "extract_command": "should not apply"
        }),
    );

    let result = apply_component_overrides(&component, &project);
    assert_eq!(result.extract_command, None);
}
