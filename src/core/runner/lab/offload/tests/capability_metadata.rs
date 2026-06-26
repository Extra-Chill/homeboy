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

    assert!(lab_runner_homeboy_has_blocking_drift(&status));

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
    assert!(err.message.contains("Active daemon: homeboy 0.0.0+test"));
    assert!(err
        .message
        .contains("configured runtime: homeboy 0.229.11+new"));
    assert!(err
        .message
        .contains("malformed or misleading provider output"));
    let tried = err.details["tried"].as_array().expect("tried hints");
    assert!(tried
        .iter()
        .any(|hint| hint.as_str().is_some_and(|hint| hint
            .contains("homeboy runner refresh-homeboy 'homeboy lab' --ref main --reconnect"))));
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("refresh or select a clean runner binary"))));
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
