use super::*;

#[test]
fn command_prefix_tools_are_included_in_capability_contract() {
    let dir = tempfile::tempdir().expect("temp dir");
    let contract = lab_runner_capability_contract(
        &portable_lab_command("lint"),
        dir.path(),
        &[RunnerRequiredTool::Cargo],
    )
    .expect("capability contract");

    assert!(contract.required_tools.contains(&RunnerRequiredTool::Cargo));
}

#[test]
fn full_workspace_lab_contract_infers_source_path_tools() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("package signal");
    std::fs::write(dir.path().join("docker-compose.yml"), "services: {}").expect("docker signal");

    let contract = lab_runner_capability_contract(
        &portable_lab_command("test"),
        dir.path(),
        &[RunnerRequiredTool::Homeboy],
    )
    .expect("capability contract");

    assert!(contract
        .required_tools
        .contains(&RunnerRequiredTool::Homeboy));
    assert!(contract.required_tools.contains(&RunnerRequiredTool::Node));
    assert!(contract.required_tools.contains(&RunnerRequiredTool::Npm));
    assert!(contract
        .required_tools
        .contains(&RunnerRequiredTool::Docker));
}

#[test]
fn workload_scoped_lab_contract_ignores_source_path_docker_signal() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("package signal");
    std::fs::write(dir.path().join("docker-compose.yml"), "services: {}").expect("docker signal");
    let mut command = portable_lab_command("trace");
    command.routing_policy.infer_source_path_tools = false;

    let contract =
        lab_runner_capability_contract(&command, dir.path(), &[RunnerRequiredTool::Homeboy])
            .expect("capability contract");

    assert!(contract
        .required_tools
        .contains(&RunnerRequiredTool::Homeboy));
    assert!(!contract.required_tools.contains(&RunnerRequiredTool::Node));
    assert!(!contract.required_tools.contains(&RunnerRequiredTool::Npm));
    assert!(!contract
        .required_tools
        .contains(&RunnerRequiredTool::Docker));
}

#[test]
fn lab_workspace_mapping_metadata_records_local_to_remote_paths() {
    let snapshot = RunnerWorkspaceSyncOutput {
        variant: "workspace_sync",
        command: "runner.workspace.sync",
        runner_id: "lab".to_string(),
        local_path: "/Users/user/Developer/app".to_string(),
        remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
        current_workspace: crate::core::runner::RunnerWorkspaceCurrentSummary {
            local_path: "/Users/user/Developer/app".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            materialized: true,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
            synthetic_checkout_commit: None,
        },
        workspace_lease: crate::core::runner::RunnerWorkspaceLease {
            runner_id: "lab".to_string(),
            local_path: "/Users/user/Developer/app".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            sync_mode: "snapshot".to_string(),
            materialized: true,
            lifecycle_owner: crate::core::runner::RunnerLifecycleOwner::Controller,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
        },
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        snapshot_identity: "snapshot:abc".to_string(),
        counts: crate::core::runner::ByteFileCounts {
            files: 3,
            bytes: 12,
        },
        excludes: Vec::new(),
        includes: Vec::new(),
        workspace_cleanliness: "snapshot_unique_workspace".to_string(),
        validation_dependencies: Vec::new(),
    };
    let git = RunnerWorkspaceSyncOutput {
        variant: "workspace_sync",
        command: "runner.workspace.sync",
        runner_id: "lab".to_string(),
        local_path: "/Users/user/Developer/dep".to_string(),
        remote_path: "/srv/homeboy/_lab_workspaces/dep-def".to_string(),
        current_workspace: crate::core::runner::RunnerWorkspaceCurrentSummary {
            local_path: "/Users/user/Developer/dep".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/dep-def".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Git,
            materialized: true,
            source_commit: Some("abc123".to_string()),
            source_ref: Some("main".to_string()),
            source_dirty: Some(false),
            synthetic_checkout_commit: None,
        },
        workspace_lease: crate::core::runner::RunnerWorkspaceLease {
            runner_id: "lab".to_string(),
            local_path: "/Users/user/Developer/dep".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/dep-def".to_string(),
            sync_mode: "git".to_string(),
            materialized: true,
            lifecycle_owner: crate::core::runner::RunnerLifecycleOwner::Controller,
            source_commit: Some("abc123".to_string()),
            source_ref: Some("main".to_string()),
            source_dirty: Some(false),
        },
        sync_mode: RunnerWorkspaceSyncMode::Git,
        snapshot_identity: "abc123".to_string(),
        counts: crate::core::runner::ByteFileCounts::default(),
        excludes: Vec::new(),
        includes: Vec::new(),
        workspace_cleanliness: "clean_remote_required".to_string(),
        validation_dependencies: Vec::new(),
    };

    let entries = vec![
        workspace_mapping_entry("primary", &snapshot),
        workspace_mapping_entry("dependency", &git),
    ];
    let metadata = lab_workspace_mapping_metadata(&entries);

    assert_eq!(metadata["schema"], LAB_WORKSPACE_MAPPING_SCHEMA);
    assert_eq!(metadata["workspaces"][0]["role"], "primary");
    assert_eq!(metadata["workspaces"][0]["sync_mode"], "snapshot");
    assert_eq!(metadata["workspaces"][1]["role"], "dependency");
    assert_eq!(metadata["workspaces"][1]["sync_mode"], "git");
    assert_eq!(
        metadata["local_to_remote"]["/Users/user/Developer/dep"],
        "/srv/homeboy/_lab_workspaces/dep-def"
    );
}

#[test]
fn lab_offload_env_contains_workspace_mapping_metadata() {
    let mapping = serde_json::json!({
        "schema": LAB_WORKSPACE_MAPPING_SCHEMA,
        "local_to_remote": {
            "/Users/user/Developer/app": "/srv/homeboy/_lab_workspaces/app-abc"
        },
        "workspaces": []
    });
    let metadata = serde_json::json!({
        "schema": "homeboy/lab-offload/v1",
        "workspace_mapping": mapping,
    });

    let env = build_lab_offload_env(&metadata);
    let parsed: serde_json::Value = serde_json::from_str(
        env.get(LAB_OFFLOAD_METADATA_ENV)
            .expect("lab offload env metadata"),
    )
    .expect("parse lab offload metadata");

    assert_eq!(parsed["workspace_mapping"], mapping);
}

#[test]
fn materialization_proof_records_hashes_source_and_runner_identity() {
    let source_snapshot = SourceSnapshot {
        runner_id: "lab".to_string(),
        local_path: Some("/Users/user/Developer/app".to_string()),
        remote_path: Some("/srv/homeboy/_lab_workspaces/app-abc".to_string()),
        workspace_root: Some("/Users/user/Developer/app".to_string()),
        git_branch: Some("main".to_string()),
        git_sha: Some("abc123".to_string()),
        dirty: false,
        sync_mode: "lab_offload".to_string(),
        workspace_snapshot_identity: Some("snapshot:workspace".to_string()),
        snapshot_hash: "sha256:source".to_string(),
        synced_at: "2026-06-21T00:00:00Z".to_string(),
        sync_excludes: vec!["target/".to_string()],
    };
    let runner_homeboy = serde_json::json!({
        "schema": "homeboy/lab-runner-homeboy/v1",
        "active_daemon_version": "homeboy 0.1.0",
    });
    let source_checkout = serde_json::json!({
        "schema": "homeboy/lab-source-checkout/v1",
        "git_sha": "abc123",
    });
    let workspace_mapping = serde_json::json!({
        "schema": LAB_WORKSPACE_MAPPING_SCHEMA,
        "workspaces": [],
        "local_to_remote": {},
    });

    let proof = lab_materialization_proof_metadata(
        &source_snapshot,
        "snapshot:workspace",
        "/srv/homeboy/_lab_workspaces/app-abc",
        &runner_homeboy,
        &source_checkout,
        &workspace_mapping,
        &[],
    );

    assert_eq!(proof["schema"], "homeboy/lab-materialization-proof/v1");
    assert_eq!(
        proof["remote_workspace"],
        "/srv/homeboy/_lab_workspaces/app-abc"
    );
    assert_eq!(
        proof["workload_hashes"]["source_snapshot_hash"],
        "sha256:source"
    );
    assert_eq!(
        proof["workload_hashes"]["workspace_snapshot_identity"],
        "snapshot:workspace"
    );
    assert_eq!(proof["source_snapshot"]["git_sha"], "abc123");
    assert_eq!(
        proof["runner_homeboy"]["active_daemon_version"],
        "homeboy 0.1.0"
    );
}

#[test]
fn lab_runner_homeboy_metadata_names_binary_and_refresh_path() {
    let status = reverse_status("homeboy lab");
    let metadata = lab_runner_homeboy_metadata(
        "homeboy lab",
        "/tmp/_lab_workspaces/homeboy/target/debug/homeboy",
        &status,
    );

    assert_eq!(metadata["schema"], "homeboy/lab-runner-homeboy/v1");
    assert_eq!(metadata["runner_id"], "homeboy lab");
    assert_eq!(metadata["controller_version"], env!("CARGO_PKG_VERSION"));
    assert!(metadata["controller_build_identity"]
        .as_str()
        .is_some_and(|identity| identity.starts_with("homeboy ")));
    assert_eq!(
        metadata["configured_executable"],
        "/tmp/_lab_workspaces/homeboy/target/debug/homeboy"
    );
    assert_eq!(metadata["active_daemon_version"], "homeboy 0.0.0");
    assert_eq!(
        metadata["active_daemon_build_identity"],
        "homeboy 0.0.0+test"
    );
    assert_eq!(metadata["version_drift"], true);
    assert_eq!(
        metadata["refresh_commands"],
        serde_json::json!([
            "homeboy runner refresh-homeboy 'homeboy lab' --ref main --reconnect",
            "homeboy runner disconnect 'homeboy lab'",
            "homeboy runner connect 'homeboy lab'"
        ])
    );
    assert_eq!(
        metadata["upgrade_command"],
        "homeboy upgrade --force --upgrade-runner 'homeboy lab'"
    );
}

#[test]
fn runner_homeboy_version_drift_blocks_offload_with_upgrade_guidance() {
    let status = reverse_status("homeboy-lab");

    // `reverse_status` reports runner `0.0.0` against the controller's current
    // version, a MINOR/MAJOR mismatch that is blocking regardless of strict mode.
    assert!(lab_runner_homeboy_has_blocking_drift(&status, false));
    assert!(lab_runner_homeboy_has_blocking_drift(&status, true));
    assert_eq!(
        classify_runner_homeboy_version_drift(&status),
        RunnerHomeboyVersionDrift::Incompatible
    );

    let err = stale_runner_homeboy_error("homeboy-lab", "homeboy", &status);

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert!(err
        .message
        .contains("Lab offload refused runner `homeboy-lab`"));
    assert!(err
        .message
        .contains("connected runner daemon reports Homeboy version `homeboy 0.0.0`"));
    assert!(err.message.contains(env!("CARGO_PKG_VERSION")));
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried
        .iter()
        .any(|hint| hint.as_str().is_some_and(|hint| hint
            .contains("homeboy runner refresh-homeboy homeboy-lab --ref main --reconnect"))));
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("refresh or select a clean runner binary"))));
}

/// Build a reverse-connected status whose runner session reports `version`.
fn status_with_runner_version(runner_id: &str, version: &str) -> RunnerStatusReport {
    let mut status = reverse_status(runner_id);
    if let Some(session) = status.session.as_mut() {
        session.homeboy_version = version.to_string();
        session.homeboy_build_identity = Some(format!("{version}+test"));
    }
    status
}

/// A version string sharing the controller's MAJOR.MINOR but a different patch.
fn same_minor_patch_drift_version(prefix: &str) -> String {
    let controller = env!("CARGO_PKG_VERSION");
    let mut parts = controller.split('.');
    let major = parts.next().unwrap_or("0");
    let minor = parts.next().unwrap_or("0");
    let patch: u64 = parts
        .next()
        .and_then(|p| {
            p.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .unwrap_or(0);
    format!("{prefix}{major}.{minor}.{}", patch.wrapping_add(1))
}

#[test]
fn same_minor_patch_drift_is_compatible_and_proceeds_with_warning() {
    let status =
        status_with_runner_version("homeboy-lab", &same_minor_patch_drift_version("homeboy "));

    assert_eq!(
        classify_runner_homeboy_version_drift(&status),
        RunnerHomeboyVersionDrift::CompatiblePatch
    );
    // Compatibility-aware default: patch drift proceeds.
    assert!(!lab_runner_homeboy_has_blocking_drift(&status, false));
    let warning = lab_runner_homeboy_compatible_drift_warning(&status, false)
        .expect("compatible patch drift should warn");
    assert!(warning.contains("wire-compatible"));
    assert!(warning.contains("require_exact_homeboy_version"));
}

#[test]
fn same_minor_patch_drift_is_refused_under_strict_mode() {
    let status = status_with_runner_version("homeboy-lab", &same_minor_patch_drift_version(""));

    // Strict override restores exact-match: patch drift now refuses.
    assert!(lab_runner_homeboy_has_blocking_drift(&status, true));
    // No "proceeding" warning is emitted under strict mode; the drift surfaces
    // as the refusal error instead.
    assert!(lab_runner_homeboy_compatible_drift_warning(&status, true).is_none());
}

#[test]
fn minor_version_drift_is_incompatible_and_refused() {
    let controller = env!("CARGO_PKG_VERSION");
    let mut parts = controller.split('.');
    let major = parts.next().unwrap_or("0");
    let minor: u64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let drifted = format!("{major}.{}.0", minor.wrapping_add(1));
    let status = status_with_runner_version("homeboy-lab", &drifted);

    assert_eq!(
        classify_runner_homeboy_version_drift(&status),
        RunnerHomeboyVersionDrift::Incompatible
    );
    assert!(lab_runner_homeboy_has_blocking_drift(&status, false));
    assert!(lab_runner_homeboy_has_blocking_drift(&status, true));
    assert!(lab_runner_homeboy_compatible_drift_warning(&status, false).is_none());
}

#[test]
fn newer_runner_than_controller_points_to_local_upgrade_first() {
    let status = status_with_runner_version("homeboy-lab", &higher_minor_version());

    let metadata = lab_runner_homeboy_metadata("homeboy-lab", "homeboy", &status);
    assert_eq!(metadata["primary_remediation_command"], "homeboy upgrade");
    assert_eq!(metadata["local_upgrade_command"], "homeboy upgrade");

    let err = stale_runner_homeboy_error("homeboy-lab", "homeboy", &status);
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried
        .first()
        .and_then(|hint| hint.as_str())
        .is_some_and(|hint| hint.contains("homeboy upgrade")));
}

#[test]
fn newer_stale_daemon_control_plane_points_to_local_upgrade_first() {
    let mut status = status_with_runner_version("homeboy-lab", env!("CARGO_PKG_VERSION"));
    let runner_version = higher_minor_version();
    status.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "homeboy-lab",
        runner_version,
        env!("CARGO_PKG_VERSION").to_string(),
        None,
        None,
    ));

    let metadata = lab_runner_homeboy_metadata("homeboy-lab", "homeboy", &status);
    assert_eq!(metadata["primary_remediation_command"], "homeboy upgrade");

    let err = stale_runner_homeboy_error("homeboy-lab", "homeboy", &status);
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried
        .first()
        .and_then(|hint| hint.as_str())
        .is_some_and(|hint| hint.contains("homeboy upgrade")));
}

#[test]
fn older_runner_than_controller_points_to_runner_refresh_first() {
    let status = status_with_runner_version("homeboy-lab", "0.0.0");

    let metadata = lab_runner_homeboy_metadata("homeboy-lab", "homeboy", &status);
    assert_eq!(
        metadata["primary_remediation_command"],
        "homeboy runner refresh-homeboy homeboy-lab --ref main --reconnect"
    );

    let err = stale_runner_homeboy_error("homeboy-lab", "homeboy", &status);
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried
        .first()
        .and_then(|hint| hint.as_str())
        .is_some_and(|hint| hint
            .contains("homeboy runner refresh-homeboy homeboy-lab --ref main --reconnect")));
}

#[test]
fn exact_version_match_has_no_drift() {
    let status = status_with_runner_version("homeboy-lab", env!("CARGO_PKG_VERSION"));

    assert_eq!(
        classify_runner_homeboy_version_drift(&status),
        RunnerHomeboyVersionDrift::None
    );
    assert!(!lab_runner_homeboy_has_blocking_drift(&status, false));
    assert!(!lab_runner_homeboy_has_blocking_drift(&status, true));
    assert!(lab_runner_homeboy_compatible_drift_warning(&status, false).is_none());
}

#[test]
fn stale_daemon_build_identity_drift_always_blocks_even_on_compatible_version() {
    // Runner version matches the controller exactly, but the runner's active
    // daemon was started by a different build than its job command binary: that
    // internal inconsistency is always blocking, independent of the version policy.
    let mut status = status_with_runner_version("homeboy-lab", env!("CARGO_PKG_VERSION"));
    status.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "homeboy-lab",
        "homeboy 0.228.0".to_string(),
        "homeboy 0.229.11".to_string(),
        Some("homeboy 0.228.0+old".to_string()),
        Some("homeboy 0.229.11+new".to_string()),
    ));

    assert_eq!(
        classify_runner_homeboy_version_drift(&status),
        RunnerHomeboyVersionDrift::None
    );
    assert!(lab_runner_homeboy_has_blocking_drift(&status, false));
}

#[test]
fn require_exact_runner_version_resolves_from_setting_and_env() {
    use crate::core::server::RunnerSettings;

    let default_settings = RunnerSettings::default();
    assert!(!require_exact_runner_version(&default_settings));

    let strict_settings = RunnerSettings {
        require_exact_homeboy_version: Some(true),
        ..RunnerSettings::default()
    };
    assert!(require_exact_runner_version(&strict_settings));

    // Env override forces strict mode even when the setting is unset/false.
    let prior = std::env::var(REQUIRE_EXACT_RUNNER_VERSION_ENV).ok();
    std::env::set_var(REQUIRE_EXACT_RUNNER_VERSION_ENV, "1");
    assert!(require_exact_runner_version(&default_settings));
    std::env::set_var(REQUIRE_EXACT_RUNNER_VERSION_ENV, "off");
    assert!(!require_exact_runner_version(&default_settings));
    match prior {
        Some(value) => std::env::set_var(REQUIRE_EXACT_RUNNER_VERSION_ENV, value),
        None => std::env::remove_var(REQUIRE_EXACT_RUNNER_VERSION_ENV),
    }
}

#[test]
fn source_checkout_ref_display_includes_branch_sha_and_dirty_state() {
    let metadata = serde_json::json!({
        "git_branch": "fix/lab-source-ref-preflight",
        "git_sha": "1234567890abcdef",
        "dirty": true,
    });

    assert_eq!(
        source_checkout_ref_display(&metadata),
        "fix/lab-source-ref-preflight@1234567890ab dirty"
    );
}

#[test]
fn source_checkout_ref_display_handles_missing_git_ref() {
    let metadata = serde_json::json!({
        "local_path": "/tmp/source",
        "dirty": null,
    });

    assert_eq!(source_checkout_ref_display(&metadata), "unknown ref");
}

#[test]
fn stale_runner_homeboy_error_blocks_offload_with_reconnect_guidance() {
    let status = stale_reverse_status("homeboy lab");

    let err = stale_runner_homeboy_error(
        "homeboy lab",
        "/home/user/Developer/_lab_workspaces/homeboy-post-4583-proof/target/debug/homeboy",
        &status,
    );

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert_eq!(err.details["field"], "runner");
    assert_eq!(err.details["id"], "homeboy lab");
    assert!(err
        .message
        .contains("Lab offload refused runner `homeboy lab`"));
    assert!(err
        .message
        .contains("/home/user/Developer/_lab_workspaces/homeboy-post-4583-proof"));
    assert!(err
        .message
        .contains("Active daemon control plane: homeboy 0.0.0+test"));
    assert!(err
        .message
        .contains("job command binary: homeboy 0.229.11+new"));
    assert!(err
        .message
        .contains("malformed or misleading provider output"));
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried
        .first()
        .and_then(|hint| hint.as_str())
        .is_some_and(|hint| hint
            .contains("homeboy runner refresh-homeboy 'homeboy lab' --ref main --reconnect")));
    assert!(tried
        .iter()
        .any(|hint| hint.as_str().is_some_and(|hint| hint
            .contains("homeboy runner refresh-homeboy 'homeboy lab' --ref main --reconnect"))));
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("refresh or select a clean runner binary"))));
}

fn higher_minor_version() -> String {
    let controller = env!("CARGO_PKG_VERSION");
    let mut parts = controller.split('.');
    let major = parts.next().unwrap_or("0");
    let minor: u64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    format!("{major}.{}.0", minor + 1)
}

#[test]
fn runner_homeboy_metadata_carries_stale_daemon_details() {
    let status = stale_reverse_status("lab");

    let metadata = lab_runner_homeboy_metadata("lab", "homeboy", &status);

    assert_eq!(
        metadata["stale_daemon"]["session_homeboy_version"],
        "homeboy 0.228.0"
    );
    assert_eq!(
        metadata["stale_daemon"]["current_homeboy_version"],
        "homeboy 0.229.11"
    );
    assert_eq!(
        metadata["stale_daemon"]["session_homeboy_build_identity"],
        "homeboy 0.228.0+old"
    );
    assert_eq!(
        metadata["stale_daemon"]["current_homeboy_build_identity"],
        "homeboy 0.229.11+new"
    );
    assert_eq!(metadata["stale_daemon"]["severity"], "warning");
    assert_eq!(
        metadata["stale_daemon"]["active_daemon_control_plane_version"],
        "homeboy 0.228.0"
    );
    assert_eq!(
        metadata["stale_daemon"]["job_command_binary_version"],
        "homeboy 0.229.11"
    );
    assert_eq!(
        metadata["stale_daemon"]["active_daemon_control_plane_build_identity"],
        "homeboy 0.228.0+old"
    );
    assert_eq!(
        metadata["stale_daemon"]["job_command_binary_build_identity"],
        "homeboy 0.229.11+new"
    );
    assert_eq!(
        metadata["stale_daemon"]["refresh_command"],
        "homeboy runner disconnect lab && homeboy runner connect lab"
    );
    assert_eq!(metadata["stale_daemon_severity"], "warning");
    assert_eq!(
        metadata["stale_daemon_refresh_command"],
        "homeboy runner disconnect lab && homeboy runner connect lab"
    );
    assert_eq!(metadata["job_command_binary_version"], "homeboy 0.229.11");
    assert_eq!(
        metadata["job_command_binary_build_identity"],
        "homeboy 0.229.11+new"
    );
    assert_eq!(
        metadata["refresh_commands"],
        serde_json::json!([
            "homeboy runner refresh-homeboy lab --ref main --reconnect",
            "homeboy runner disconnect lab",
            "homeboy runner connect lab"
        ])
    );
}

#[test]
fn runner_homeboy_daemon_display_prefers_build_identity() {
    let metadata = serde_json::json!({
        "active_daemon_version": "homeboy 0.1.0",
        "active_daemon_build_identity": "homeboy 0.1.0+abc123",
    });

    assert_eq!(
        runner_homeboy_daemon_display(&metadata),
        "homeboy 0.1.0+abc123"
    );
}
