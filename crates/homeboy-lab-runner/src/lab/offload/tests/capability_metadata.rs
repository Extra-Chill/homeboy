use super::*;
use homeboy_core::error::posix_quote_arg;

fn admission_capability(
    command: &LabOffloadCommand,
    source_path: &std::path::Path,
    required_tools: Vec<RunnerRequiredTool>,
) -> crate::PreparedLabRunnerCapability {
    compile_lab_admission_plan(command, source_path, &required_tools)
        .expect("compile routed admission")
        .capability
}

#[test]
fn command_prefix_tools_are_included_in_capability_contract() {
    let dir = tempfile::tempdir().expect("temp dir");
    let contract = admission_capability(
        &portable_lab_command("lint"),
        dir.path(),
        vec![RunnerRequiredTool::new("compiler")],
    );

    assert!(contract
        .required_tools
        .contains(&RunnerRequiredTool::new("compiler")));
}

#[test]
fn lab_contract_uses_declared_tools_only_regardless_of_source_signals() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("package signal");
    std::fs::write(dir.path().join("docker-compose.yml"), "services: {}").expect("docker signal");

    for (label, infer_source_path_tools) in [("test", true), ("trace", false)] {
        let mut command = portable_lab_command(label);
        command.routing_policy.infer_source_path_tools = infer_source_path_tools;

        let contract =
            admission_capability(&command, dir.path(), vec![RunnerRequiredTool::homeboy()]);

        assert!(contract
            .required_tools
            .contains(&RunnerRequiredTool::homeboy()));
        assert_eq!(contract.required_tools.len(), 2, "{label}");
    }
}

#[test]
fn lab_workspace_mapping_metadata_records_local_to_remote_paths() {
    let snapshot = RunnerWorkspaceSyncOutput {
        variant: "workspace_sync",
        command: "runner.workspace.sync",
        runner_id: "lab".to_string(),
        local_path: "/Users/user/Developer/app".to_string(),
        remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
        materialization_plan: RunnerWorkspaceMaterializationPlan::from_test_parts(
            "/srv/homeboy",
            "/Users/user/Developer/app",
            "app",
            "/srv/homeboy/_lab_workspaces/app-abc",
            RunnerWorkspaceSyncMode::Snapshot,
            "snapshot:abc",
        ),
        current_workspace: crate::RunnerWorkspaceCurrentSummary {
            local_path: "/Users/user/Developer/app".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            materialized: true,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
            synthetic_checkout_commit: None,
            synthetic_checkout_ref: None,
            synthetic_checkout_tree: None,
        },
        workspace_lease: crate::RunnerWorkspaceLease {
            runner_id: "lab".to_string(),
            local_path: "/Users/user/Developer/app".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            sync_mode: "snapshot".to_string(),
            materialized: true,
            lifecycle_owner: crate::RunnerLifecycleOwner::Controller,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
        },
        resource_lifecycle: crate::workspace_resource_lifecycle(
            "lab",
            "/srv/homeboy/_lab_workspaces/app-abc",
            None,
            homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
        ),
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        snapshot_identity: "snapshot:abc".to_string(),
        content: None,
        workspace_ref: "workspace:00000000-0000-0000-0000-000000000001".to_string(),
        prepared_workspace_lease: None,
        counts: crate::ByteFileCounts {
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
        materialization_plan: RunnerWorkspaceMaterializationPlan::from_test_parts(
            "/srv/homeboy",
            "/Users/user/Developer/dep",
            "dep",
            "/srv/homeboy/_lab_workspaces/dep-def",
            RunnerWorkspaceSyncMode::Git,
            "abc123",
        ),
        current_workspace: crate::RunnerWorkspaceCurrentSummary {
            local_path: "/Users/user/Developer/dep".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/dep-def".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Git,
            materialized: true,
            source_commit: Some("abc123".to_string()),
            source_ref: Some("main".to_string()),
            source_dirty: Some(false),
            synthetic_checkout_commit: None,
            synthetic_checkout_ref: None,
            synthetic_checkout_tree: None,
        },
        workspace_lease: crate::RunnerWorkspaceLease {
            runner_id: "lab".to_string(),
            local_path: "/Users/user/Developer/dep".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/dep-def".to_string(),
            sync_mode: "git".to_string(),
            materialized: true,
            lifecycle_owner: crate::RunnerLifecycleOwner::Controller,
            source_commit: Some("abc123".to_string()),
            source_ref: Some("main".to_string()),
            source_dirty: Some(false),
        },
        resource_lifecycle: crate::workspace_resource_lifecycle(
            "lab",
            "/srv/homeboy/_lab_workspaces/dep-def",
            None,
            homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
        ),
        sync_mode: RunnerWorkspaceSyncMode::Git,
        snapshot_identity: "abc123".to_string(),
        content: None,
        workspace_ref: "workspace:00000000-0000-0000-0000-000000000002".to_string(),
        prepared_workspace_lease: None,
        counts: crate::ByteFileCounts::default(),
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
fn lab_offload_env_projects_only_subprocess_provenance() {
    let metadata = serde_json::json!({
        "schema": "homeboy/lab-offload/v1",
        "runner_id": "lab",
        "status": "offloaded",
        "remote_workspace": "/srv/homeboy/workspace",
        "sync_mode": "snapshot",
        "source_snapshot": { "runner_id": "lab" },
        "workspace_verification": {
            "schema": "homeboy/lab-workspace-verification/v2",
            "identity": "snapshot:abc",
            "content_hash": "sha256:abc",
            "content_manifest": { "entries": ["x".repeat(2_000_000)] },
            "primary_workspace": {
                "identity": "snapshot:abc",
                "remote_path": "/srv/homeboy/workspace",
                "local_path": "/controller/workspace"
            }
        },
    });

    let env = build_lab_offload_env(&metadata);
    let parsed: serde_json::Value = serde_json::from_str(
        env.get(LAB_OFFLOAD_METADATA_ENV)
            .expect("lab offload env metadata"),
    )
    .expect("parse lab offload metadata");

    assert_eq!(parsed["schema"], "homeboy/lab-offload-subprocess/v1");
    assert_eq!(parsed["dispatch_schema"], "homeboy/lab-offload/v1");
    assert_eq!(parsed["runner_id"], "lab");
    assert_eq!(parsed["source_snapshot"], metadata["source_snapshot"]);
    assert_eq!(
        parsed["workspace_verification"]["primary_workspace"]["remote_path"],
        "/srv/homeboy/workspace"
    );
    assert!(parsed
        .pointer("/workspace_verification/content_manifest")
        .is_none());
    assert_eq!(
        metadata["workspace_verification"]["content_manifest"]["entries"][0]
            .as_str()
            .expect("durable metadata keeps the full audit manifest")
            .len(),
        2_000_000
    );
    assert!(
        env.get(LAB_OFFLOAD_METADATA_ENV)
            .expect("bounded subprocess metadata")
            .len()
            < 16 * 1024
    );
}

#[test]
fn lab_offload_workspace_verification_metadata_survives_process_env_hydration() {
    let source = tempfile::tempdir().expect("source workspace");
    let remote = tempfile::tempdir().expect("materialized workspace");
    std::fs::write(source.path().join("README.md"), "verified contents\n").expect("source file");
    std::fs::write(remote.path().join("README.md"), "verified contents\n").expect("remote file");
    std::fs::write(source.path().join("excluded.txt"), "controller only\n").expect("excluded file");

    let source_path = source.path().canonicalize().expect("canonical source");
    let remote_path = remote.path().canonicalize().expect("canonical remote");
    let snapshot = SourceSnapshot {
        runner_id: "lab".to_string(),
        local_path: Some(source_path.display().to_string()),
        remote_path: Some(remote_path.display().to_string()),
        workspace_root: Some(source_path.display().to_string()),
        git_branch: Some("main".to_string()),
        git_sha: Some("a".repeat(40)),
        dirty: false,
        sync_mode: "lab_offload".to_string(),
        workspace_snapshot_identity: Some("snapshot:verified".to_string()),
        prepared_workspace_original_snapshot_identity: None,
        prepared_workspace_update_lineage: Vec::new(),
        synthetic_checkout_commit: None,
        synthetic_checkout_ref: None,
        synthetic_checkout_tree: None,
        snapshot_hash: "sha256:verified".to_string(),
        synced_at: "2026-07-14T00:00:00Z".to_string(),
        sync_excludes: vec!["excluded.txt".to_string()],
        repository_integrity_evidence: None,
    };
    let synced_workspace = primary_synced_workspace(&source_path, &remote_path);
    let path_materialization_plan = PathMaterializationPlan::new([PathMaterializationEntry::new(
        "primary",
        PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT,
        Some(source_path.display().to_string()),
        remote_path.display().to_string(),
        PATH_MATERIALIZATION_MODE_SNAPSHOT,
        PATH_MATERIALIZATION_STATUS_MATERIALIZED,
    )]);
    let mut plan = base_lab_plan(None);
    plan.steps.push(
        homeboy_core::plan::PlanStep::ready("lab.sync_workspace", "lab.sync_workspace")
            .inputs(homeboy_core::plan::PlanValues::new().string("mode", "snapshot"))
            .build(),
    );
    let mut metadata = crate::lab_offload_metadata(
        &plan,
        "explicit",
        Some("lab"),
        Some("direct_ssh"),
        "offloaded",
        Some(&remote_path.display().to_string()),
        None,
    );
    let mut missing_inventory = synced_workspace.clone();
    missing_inventory.content = None;
    assert!(attach_lab_workspace_metadata(
        &mut metadata,
        LabWorkspaceMetadataInputs {
            source_snapshot: &snapshot,
            workspace_snapshots: std::slice::from_ref(&snapshot),
            legacy_path_materialization_plan: &path_materialization_plan,
            primary_synced_workspace: &missing_inventory,
            extra_synced_workspaces: &[],
        },
    )
    .expect_err("missing sync inventory must not trigger a recompute")
    .message
    .contains("no content inventory"));
    attach_lab_workspace_metadata(
        &mut metadata,
        LabWorkspaceMetadataInputs {
            source_snapshot: &snapshot,
            workspace_snapshots: &[snapshot.clone()],
            legacy_path_materialization_plan: &path_materialization_plan,
            primary_synced_workspace: &synced_workspace,
            extra_synced_workspaces: &[],
        },
    )
    .expect("build verifier metadata");

    let env = build_lab_offload_env(&metadata);
    let prior_lab = std::env::var(LAB_OFFLOAD_METADATA_ENV).ok();
    let prior_snapshot = std::env::var(SOURCE_SNAPSHOT_METADATA_ENV).ok();
    std::env::set_var(
        LAB_OFFLOAD_METADATA_ENV,
        env.get(LAB_OFFLOAD_METADATA_ENV)
            .expect("Lab metadata process env"),
    );
    std::env::set_var(
        SOURCE_SNAPSHOT_METADATA_ENV,
        serde_json::to_string(&snapshot).expect("source snapshot process env"),
    );

    let verified = verify_lab_workspace_from_env(&remote_path.display().to_string(), remote.path())
        .expect("verifier accepts hydrated Lab process metadata");
    assert_eq!(verified.workspace_identity, "snapshot:verified");
    let mut compact_metadata = metadata.clone();
    compact_metadata["workspace_verification"]
        .as_object_mut()
        .expect("workspace verification object")
        .remove("content_manifest");
    std::env::set_var(
        LAB_OFFLOAD_METADATA_ENV,
        serde_json::to_string(&compact_metadata).expect("compact Lab metadata process env"),
    );
    verify_lab_workspace_from_env(&remote_path.display().to_string(), remote.path())
        .expect("content hash verifies when optional manifest diagnostics are excluded");
    assert_eq!(
        metadata["workspace_verification"]["identity"],
        "snapshot:verified"
    );
    assert_eq!(
        metadata["workspace_verification"]["sync_excludes"],
        serde_json::json!(["excluded.txt"])
    );
    let serialized_metadata = serde_json::to_string(&metadata).expect("metadata JSON");
    assert!(!serialized_metadata.contains("verified contents"));

    std::fs::write(remote.path().join("README.md"), "changed contents\n")
        .expect("change remote file");
    let error = verify_lab_workspace_from_env(&remote_path.display().to_string(), remote.path())
        .expect_err("changed remote content must fail verification");
    assert!(
        error.contains(
            crate::workspace_content_hash_algorithm(
                crate::WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
            )
            .expect("default content hash algorithm")
            .as_str()
        )
    );
    assert!(error.contains("expected sha256:"));
    assert!(error.contains("got sha256:"));
    assert!(error.contains("homeboy runner workspace sync --mode snapshot"));

    match prior_lab {
        Some(value) => std::env::set_var(LAB_OFFLOAD_METADATA_ENV, value),
        None => std::env::remove_var(LAB_OFFLOAD_METADATA_ENV),
    }
    match prior_snapshot {
        Some(value) => std::env::set_var(SOURCE_SNAPSHOT_METADATA_ENV, value),
        None => std::env::remove_var(SOURCE_SNAPSHOT_METADATA_ENV),
    }
}

fn primary_synced_workspace(
    local_path: &std::path::Path,
    remote_path: &std::path::Path,
) -> RunnerWorkspaceSyncOutput {
    let local_path = local_path.display().to_string();
    let remote_path = remote_path.display().to_string();
    RunnerWorkspaceSyncOutput {
        variant: "workspace_sync",
        command: "runner.workspace.sync",
        runner_id: "lab".to_string(),
        local_path: local_path.clone(),
        remote_path: remote_path.clone(),
        materialization_plan: RunnerWorkspaceMaterializationPlan::from_test_parts(
            "/srv/homeboy",
            &local_path,
            "app",
            &remote_path,
            RunnerWorkspaceSyncMode::Snapshot,
            "snapshot:verified",
        ),
        current_workspace: crate::RunnerWorkspaceCurrentSummary {
            local_path: local_path.clone(),
            remote_path: remote_path.clone(),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            materialized: true,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
            synthetic_checkout_commit: None,
            synthetic_checkout_ref: None,
            synthetic_checkout_tree: None,
        },
        workspace_lease: crate::RunnerWorkspaceLease {
            runner_id: "lab".to_string(),
            local_path: local_path.clone(),
            remote_path: remote_path.clone(),
            sync_mode: "snapshot".to_string(),
            materialized: true,
            lifecycle_owner: crate::RunnerLifecycleOwner::Controller,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
        },
        resource_lifecycle: crate::workspace_resource_lifecycle(
            "lab",
            &remote_path,
            None,
            homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
        ),
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        snapshot_identity: "snapshot:verified".to_string(),
        content: Some((
            crate::workspace_content_manifest_for_policy(
                Path::new(&local_path),
                &["excluded.txt".to_string()],
                crate::WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
            )
            .expect("source manifest"),
            crate::workspace_content_hash(Path::new(&local_path), &["excluded.txt".to_string()])
                .expect("source digest"),
        )),
        workspace_ref: "workspace:00000000-0000-0000-0000-000000000003".to_string(),
        prepared_workspace_lease: None,
        counts: crate::ByteFileCounts::default(),
        excludes: vec!["excluded.txt".to_string()],
        includes: Vec::new(),
        workspace_cleanliness: "snapshot_unique_workspace".to_string(),
        validation_dependencies: Vec::new(),
    }
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
        prepared_workspace_original_snapshot_identity: None,
        prepared_workspace_update_lineage: Vec::new(),
        synthetic_checkout_commit: None,
        synthetic_checkout_ref: None,
        synthetic_checkout_tree: None,
        snapshot_hash: "sha256:source".to_string(),
        synced_at: "2026-06-21T00:00:00Z".to_string(),
        sync_excludes: vec!["target/".to_string()],
        repository_integrity_evidence: None,
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
    let status = connected_status("homeboy lab");
    let metadata = lab_runner_homeboy_metadata(
        "homeboy lab",
        "/tmp/_lab_workspaces/homeboy/target/debug/homeboy",
        &status,
    );

    assert_eq!(metadata["schema"], "homeboy/lab-runner-homeboy/v1");
    assert_eq!(
        metadata["configured_executable"],
        "/tmp/_lab_workspaces/homeboy/target/debug/homeboy"
    );
    assert_eq!(
        metadata["active_daemon_build_identity"],
        "homeboy 0.0.0+test"
    );
    assert_eq!(metadata["version_drift"], true);
    assert_eq!(metadata["refresh_commands"], serde_json::json!([]));
    assert_eq!(
        metadata["upgrade_command"],
        "homeboy upgrade --force --upgrade-runner 'homeboy lab'"
    );
}

/// Exercise the public handshake, including the real HTTP observation, rather
/// than constructing a negotiation record that would hide probe failures.
#[test]
fn admission_distinguishes_unavailable_probe_from_missing_capability() {
    use homeboy_lab_contract::{negotiate_lab_capability_handshake, LabAdmissionRejection};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    for (http_status, requests, expected) in [
        (
            "503 Service Unavailable",
            3,
            LabAdmissionRejection::AdmissionProbeUnavailable,
        ),
        ("200 OK", 1, LabAdmissionRejection::CapabilityIncompatible),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let count = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..count]).starts_with("GET /version "));
                let body = r#"{"lab_handoff_capabilities":[]}"#;
                write!(stream, "HTTP/1.1 {http_status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let runner = crate::Runner {
            id: "probe-regression".to_string(),
            kind: crate::RunnerKind::Local,
            server_id: None,
            workspace_root: None,
            settings: Default::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: Default::default(),
        };
        let mut status = connected_status(&runner.id);
        status.session.as_mut().unwrap().local_url = Some(url);
        let result = crate::runners::runner_capability_handshake(&runner, &status, "homeboy");
        let error = match result {
            Err(error) => error,
            Ok(handshake) => {
                let admission = negotiate_lab_capability_handshake(&handshake);
                assert_eq!(admission.rejection_code, Some(expected));
                capability_admission_error(&runner.id, &admission, false)
            }
        };
        assert_eq!(
            error.details["admission_cause_class"],
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(error.retryable == Some(true), requests == 3);
        if requests == 3 {
            assert!(error.details["probe_error"]
                .as_str()
                .unwrap()
                .contains("503"));
            assert!(!error.message.contains("Upgrade or reconnect"));
        } else {
            assert!(error.message.contains("Upgrade or reconnect"));
        }
        server.join().unwrap();
    }
}

#[test]
fn strict_capability_admission_accepts_only_compatible_exact_source() {
    use homeboy_lab_contract::{
        LabCapabilityAdmission, LabCapabilityNegotiationProvenance, LabRuntimeAncestry,
        LabRuntimeIdentity,
    };

    let identity = LabRuntimeIdentity {
        build_identity: "homeboy 0.348.7+5ebb24a09bc5".to_string(),
        lab_contract_version: None,
        source_revision: "5ebb24a09bc5".to_string(),
        clean: true,
    };
    let admission = |compatible, ancestry| LabCapabilityAdmission {
        compatible,
        rejection_code: None,
        provenance: LabCapabilityNegotiationProvenance {
            schema: "homeboy/lab-capability-negotiation/v1".to_string(),
            controller_requirement: identity.clone(),
            executed_runner_command: identity.clone(),
            active_daemon: identity.clone(),
            ancestry,
            negotiated_capabilities: Vec::new(),
            compatible,
            rejection_reason: (!compatible).then(|| "incompatible".to_string()),
        },
    };

    let exact = admission(true, LabRuntimeAncestry::ExactSource);
    assert!(!capability_admission_has_blocking_drift(&exact, true));
    assert!(!capability_admission_has_blocking_drift(&exact, false));

    let mut newer = admission(true, LabRuntimeAncestry::VerifiedNewerDescendant);
    newer.provenance.executed_runner_command.build_identity = "different".to_string();
    assert!(capability_admission_has_blocking_drift(&newer, true));
    assert!(!capability_admission_has_blocking_drift(&newer, false));

    let incompatible = admission(false, LabRuntimeAncestry::ExactSource);
    assert!(capability_admission_has_blocking_drift(&incompatible, true));
    assert!(capability_admission_has_blocking_drift(
        &incompatible,
        false
    ));
}

/// Build a connected status whose runner session reports `version`.
fn status_with_runner_version(runner_id: &str, version: &str) -> RunnerStatusReport {
    let mut status = connected_status(runner_id);
    if let Some(session) = status.session.as_mut() {
        session.homeboy_version = version.to_string();
        session.homeboy_build_identity = None;
    }
    status
}

#[test]
fn unverified_version_drift_never_advertises_runner_refresh() {
    let mut newer_stale_daemon =
        status_with_runner_version("homeboy-lab", homeboy_product_identity::product_version());
    newer_stale_daemon.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "homeboy-lab",
        higher_minor_version(),
        homeboy_product_identity::product_version().to_string(),
        None,
        None,
    ));

    for (label, status, configured) in [
        ("build drift", connected_status("homeboy-lab"), "homeboy"),
        (
            "newer runner",
            status_with_runner_version("homeboy-lab", &higher_minor_version()),
            "homeboy",
        ),
        ("newer stale daemon", newer_stale_daemon, "homeboy"),
        (
            "older runner",
            status_with_runner_version("homeboy-lab", "0.0.0"),
            "homeboy",
        ),
        (
            "configured binary drift",
            status_with_runner_version("homeboy-lab", "0.0.0"),
            "/srv/homeboy-current/target/release/homeboy",
        ),
    ] {
        let metadata = lab_runner_homeboy_metadata("homeboy-lab", configured, &status);
        assert_eq!(
            metadata["primary_remediation_command"],
            serde_json::Value::Null,
            "{label}"
        );
        assert_eq!(
            metadata["topology_recovery_command"],
            serde_json::Value::Null,
            "{label}"
        );

        let err = stale_runner_homeboy_error("homeboy-lab", configured, &status);
        assert!(err
            .message
            .contains("Lab offload refused runner `homeboy-lab`"));
        let tried = err.details["tried"].as_array().expect("tried hints");
        assert!(
            tried.iter().all(|hint| hint
                .as_str()
                .is_some_and(|hint| !hint.contains("refresh-homeboy"))),
            "{label}: {tried:?}"
        );
        assert!(
            tried.iter().any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("no ancestry-verified refresh target"))),
            "{label}: {tried:?}"
        );
    }
}

#[test]
fn controller_recovery_command_follows_the_binary_install_method() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
    std::fs::create_dir_all(dir.path().join("target/release")).expect("target dir");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"homeboy\"",
    )
    .expect("cargo manifest");
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").expect("main source");

    let command =
        controller_homeboy_recovery_command_for_binary(&dir.path().join("target/release/homeboy"));

    assert_eq!(
        command,
        format!(
            "homeboy upgrade --method source --source-path {} --force",
            posix_quote_arg(&dir.path().display().to_string())
        )
    );

    for (binary, expected) in [
        (
            "/opt/homebrew/Cellar/homeboy/0.276.0/bin/homeboy",
            "homeboy upgrade --method homebrew --force",
        ),
        ("/Users/user/.cargo/bin/homeboy", "homeboy upgrade"),
    ] {
        assert_eq!(
            controller_homeboy_recovery_command_for_binary(std::path::Path::new(binary)),
            expected
        );
    }
}

#[test]
fn build_identity_drift_does_not_block_default_admission() {
    // Direct SSH admission compares the daemon with the runner-side executable
    // that will execute jobs, never with the controller's target-specific binary.
    let mut direct = status_with_runner_version("homeboy-lab", "0.295.0");
    let session = direct.session.as_mut().expect("runner session");
    session.mode = RunnerTunnelMode::DirectSsh;
    session.homeboy_build_identity = Some("homeboy 0.295.0+b03d659866d1".to_string());

    let mut daemon_drift =
        status_with_runner_version("homeboy-lab", homeboy_product_identity::product_version());
    daemon_drift.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "homeboy-lab",
        "homeboy 0.228.0".to_string(),
        "homeboy 0.229.11".to_string(),
        Some("homeboy 0.228.0+old".to_string()),
        Some("homeboy 0.229.11+new".to_string()),
    ));

    for status in [direct, daemon_drift] {
        assert!(!lab_runner_homeboy_has_blocking_status_drift(
            &status, false
        ));
    }
}

#[test]
fn require_exact_runner_version_resolves_from_setting_and_env() {
    use homeboy_core::server::RunnerSettings;

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
fn source_checkout_ref_display_summarizes_available_git_state() {
    for (metadata, expected) in [
        (
            serde_json::json!({
                "git_branch": "fix/lab-source-ref-preflight",
                "git_sha": "1234567890abcdef",
                "dirty": true,
            }),
            "fix/lab-source-ref-preflight@1234567890ab dirty",
        ),
        (
            serde_json::json!({ "local_path": "/tmp/source", "dirty": null }),
            "unknown ref",
        ),
    ] {
        assert_eq!(source_checkout_ref_display(&metadata), expected);
    }
}

#[test]
fn stale_runner_homeboy_error_blocks_offload_with_reconnect_guidance() {
    let status = stale_connected_status("homeboy lab");

    let err = stale_runner_homeboy_error(
        "homeboy lab",
        "/home/user/Developer/_lab_workspaces/homeboy-post-4583-proof/target/debug/homeboy",
        &status,
    );

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert_eq!(
        err.message,
        "Invalid argument 'runner': Lab offload refused runner `homeboy lab` because its active daemon control plane differs from the configured job command binary `/home/user/Developer/_lab_workspaces/homeboy-post-4583-proof/target/debug/homeboy`. Active daemon control plane: homeboy 0.0.0+test; job command binary: homeboy 0.229.11+new. connected runner daemon control plane version `homeboy 0.228.0` differs from configured job command binary version `homeboy 0.229.11` on runner `homeboy lab`; build drift is informational Stale runner runtimes can return malformed or misleading provider output; follow the first remediation hint before retrying."
    );
}

fn higher_minor_version() -> String {
    let controller = homeboy_product_identity::product_version();
    let mut parts = controller.split('.');
    let major = parts.next().unwrap_or("0");
    let minor: u64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    format!("{major}.{}.0", minor + 1)
}

#[test]
fn runner_homeboy_metadata_carries_stale_daemon_details() {
    let status = stale_connected_status("lab");

    let metadata = lab_runner_homeboy_metadata("lab", "homeboy", &status);

    assert_eq!(
        metadata["stale_daemon"]["active_daemon_control_plane_build_identity"],
        "homeboy 0.228.0+old"
    );
    assert_eq!(
        metadata["stale_daemon"]["job_command_binary_build_identity"],
        "homeboy 0.229.11+new"
    );
    assert!(metadata["stale_daemon"].get("refresh_command").is_none());
    assert_eq!(metadata["stale_daemon_severity"], "info");
    assert_eq!(metadata["job_command_binary_version"], "homeboy 0.229.11");
    assert_eq!(
        metadata["job_command_binary_build_identity"],
        "homeboy 0.229.11+new"
    );
    assert_eq!(metadata["refresh_commands"], serde_json::json!([]));
}

#[test]
fn exact_active_daemon_target_remains_available_in_metadata_and_offload_error() {
    let mut status = connected_status("lab");
    status.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "lab",
        "0.264.0".to_string(),
        "0.265.0".to_string(),
        Some("homeboy 0.265.0+aaaaaaaa".to_string()),
        Some("homeboy 0.265.0+aaaaaaaa".to_string()),
    ));
    let refresh = format!(
        "homeboy runner refresh-homeboy lab --ref {}",
        crate::controller_refresh_ref()
    );

    status.stale_daemon = status.stale_daemon.take().map(|warning| {
        warning.with_runtime_paths(
            "lab",
            Vec::new(),
            vec![crate::RunnerChangedRuntimePath {
                env: "HOMEBOY_PLUGIN_PATH".into(),
                loaded_path: Some("/old".into()),
                configured_path: Some("/new".into()),
            }],
        )
    });
    let metadata = lab_runner_homeboy_metadata("lab", "homeboy", &status);
    assert_eq!(metadata["stale_daemon_refresh_command"], refresh);
    assert_eq!(metadata["stale_daemon"]["refresh_command"], refresh);
    assert_eq!(metadata["refresh_commands"], serde_json::json!([refresh]));

    let error = stale_runner_homeboy_error("lab", "homeboy", &status);
    assert!(error.details["tried"].as_array().is_some_and(|tried| {
        tried
            .iter()
            .any(|hint| hint.as_str().is_some_and(|hint| hint.contains(&refresh)))
    }));
}
