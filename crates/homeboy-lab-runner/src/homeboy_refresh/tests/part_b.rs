#![cfg(test)]

use super::*;
use crate::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
use homeboy_core::test_support;
use std::time::{Duration, Instant};

#[test]
fn materialized_identity_rejects_dirty_display_when_state_is_unknown() {
    let plan = ssh_bootstrap_plan();

    for display in [
        "homeboy 0.284.1+abc123-dirty",
        "homeboy 0.284.1+abc123 (dirty)",
    ] {
        let identity = serde_json::json!({
            "data": {
                "version": "0.284.1",
                "git_commit": "abc123",
                "display": display
            }
        });
        let error =
            verify_materialized_identity(&plan, "HOMEBOY_REFRESH_SOURCE_SHA=abc123\n", &identity)
                .expect_err("dirty display is rejected");

        assert!(error.contains("not a canonical clean build"));
    }
}

#[test]
fn materialized_identity_requires_commit_and_matching_source_sha() {
    let plan = ssh_bootstrap_plan();
    let missing_commit = serde_json::json!({
        "data": { "git_dirty": false }
    });
    let missing_error = verify_materialized_identity(
        &plan,
        "HOMEBOY_REFRESH_SOURCE_SHA=abc123\n",
        &missing_commit,
    )
    .expect_err("commit is required");
    assert!(missing_error.contains("did not report git_commit"));

    let mismatch = serde_json::json!({
        "data": { "git_commit": "def456", "git_dirty": false }
    });
    let mismatch_error =
        verify_materialized_identity(&plan, "HOMEBOY_REFRESH_SOURCE_SHA=abc123\n", &mismatch)
            .expect_err("source SHA mismatch is rejected");
    assert!(mismatch_error.contains("does not match resolved ref"));
}

#[test]
fn refreshed_runner_env_replaces_stale_control_plane_overrides() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{
                "id": "lab-local",
                "kind": "local",
                "workspace_root": "/runner/ws",
                "homeboy_path": "/old/homeboy",
                "env": {
                    "PATH": "/usr/bin:/bin",
                    "RUST_LOG": "info",
                    "HOMEBOY_COMMAND": "/old/homeboy",
                    "HOMEBOY_DAEMON_STATE_DIR": "/old/daemon-state"
                }
            }"#,
            false,
        )
        .expect("create runner");

        let env = refreshed_runner_env(
            "lab-local",
            "/runner/ws/_homeboy_binaries/homeboy-main/target/release/homeboy",
        )
        .expect("refresh env");

        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some("/runner/ws/_homeboy_binaries/homeboy-main/target/release:/usr/bin:/bin")
        );
        assert_eq!(env.get("RUST_LOG").map(String::as_str), Some("info"));
        assert_eq!(
            env.get("HOMEBOY_COMMAND").map(String::as_str),
            Some("/runner/ws/_homeboy_binaries/homeboy-main/target/release/homeboy")
        );
        assert_eq!(env.get("HOMEBOY_DAEMON_STATE_DIR"), None);

        promote_verified_runner_binary_with(
            || Ok(()),
            "lab-local",
            "/runner/ws/_homeboy_binaries/homeboy-main/target/release/homeboy",
            |runner_id, homeboy_path| {
                let patch = refreshed_runner_patch(runner_id, homeboy_path)?;
                match merge(Some(runner_id), &patch.to_string(), &[])? {
                    MergeOutput::Single(result) => Ok(result.updated_fields),
                    MergeOutput::Bulk(_) => Ok(Vec::new()),
                }
            },
            |runner_id| Ok(crate::load(runner_id)?.settings.homeboy_path),
        )
        .expect("promote refreshed binary");
        let offload_env = crate::effective_env("lab-local").expect("effective offload env");
        assert_eq!(
            offload_env.get("HOMEBOY_COMMAND").map(String::as_str),
            Some("/runner/ws/_homeboy_binaries/homeboy-main/target/release/homeboy")
        );
        assert_eq!(offload_env.get("HOMEBOY_DAEMON_STATE_DIR"), None);
    });
}

#[test]
fn dev_binary_path_uses_content_hash_slot() {
    assert_eq!(
        dev_binary_path("/runner/ws/", "0123456789abcdef9999"),
        "/runner/ws/_homeboy_binaries/dev/0123456789abcdef/homeboy"
    );
}

#[test]
fn extension_overlay_plan_uses_content_hash_slot() {
    let dir = tempfile::tempdir().expect("extension source");
    std::fs::write(dir.path().join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");
    std::fs::write(dir.path().join("run.sh"), "echo hi\n").expect("source");

    let plan = plan_extension_overlays("/runner/ws/", &[format!("rust={}", dir.path().display())])
        .expect("overlay plan");

    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].id, "rust");
    assert!(plan[0]
        .synced_source_path
        .starts_with("/runner/ws/_lab_workspaces/dev-extensions/rust/"));
    assert!(plan[0].synced_source_path.ends_with('/'));
}

#[test]
fn dev_sync_resource_replaces_existing_extension_overlay_by_id() {
    let existing = serde_json::json!({
        "schema": "homeboy/runner-dev-sync/v1",
        "homeboy": {"hash": "old-binary"},
        "extensions": [
            {"id": "nodejs", "source_path": "/old/nodejs", "content_hash": "old"},
            {"id": "rust", "source_path": "/extensions/rust", "content_hash": "rust-hash"}
        ]
    });
    let extension =
        super::super::super::extension_materialization::RunnerExtensionMaterializationProvenance {
            id: "nodejs".to_string(),
            source_path: "/new/nodejs".to_string(),
            synced_source_path: "/runner/ws/_lab_workspaces/dev-extensions/nodejs/newhash/"
                .to_string(),
            content_hash: "new".to_string(),
            source_revision: None,
            dirty: false,
            dirty_fingerprint: None,
            synced_at: "2026-07-07T00:00:00Z".to_string(),
            dev_overlay: true,
            lifecycle: super::super::super::extension_materialization::dev_extension_lifecycle(
                "lab",
                "/runner/ws/_lab_workspaces/dev-extensions/nodejs/newhash/",
                "nodejs",
            ),
            materialization_source: None,
        };

    let updated = updated_dev_sync_resource(Some(existing), None, &[extension])
        .expect("updates dev-sync resource");
    let extensions = updated["extensions"].as_array().expect("extensions array");

    assert_eq!(updated["homeboy"]["hash"], "old-binary");
    assert_eq!(extensions.len(), 2);
    assert_eq!(extensions[0]["id"], "rust");
    assert_eq!(extensions[1]["id"], "nodejs");
    assert_eq!(extensions[1]["source_path"], "/new/nodejs");
    assert_eq!(extensions[1]["content_hash"], "new");
}

#[test]
fn dev_sync_resource_keeps_last_duplicate_overlay_for_same_id() {
    let existing = serde_json::json!({
        "schema": "homeboy/runner-dev-sync/v1",
        "extensions": [
            {"id": "nodejs", "source_path": "/old/nodejs", "content_hash": "old"},
            {"id": "nodejs", "source_path": "/newer/nodejs", "content_hash": "newer"}
        ]
    });

    let updated =
        updated_dev_sync_resource(Some(existing), None, &[]).expect("normalizes dev-sync resource");
    let extensions = updated["extensions"].as_array().expect("extensions array");

    assert_eq!(extensions.len(), 1);
    assert_eq!(extensions[0]["source_path"], "/newer/nodejs");
    assert_eq!(extensions[0]["content_hash"], "newer");
}

#[test]
fn dev_sync_resource_replacement_persists_reconciled_overlay_records() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{
                "id": "lab-local",
                "kind": "local",
                "workspace_root": "/runner/ws",
                "resources": {
                    "dev_sync": {
                        "schema": "homeboy/runner-dev-sync/v1",
                        "extensions": [
                            {"id": "nodejs", "source_path": "/old/nodejs", "content_hash": "old"},
                            {"id": "nodejs", "source_path": "/newer/nodejs", "content_hash": "newer"}
                        ]
                    }
                }
            }"#,
            false,
        )
        .expect("create runner");

        let runner = crate::load("lab-local").expect("load runner");
        let dev_sync =
            updated_dev_sync_resource(runner.resources.get("dev_sync").cloned(), None, &[])
                .expect("reconcile dev-sync resource");
        let patch = serde_json::json!({ "resources": { "dev_sync": dev_sync } });

        crate::merge(
            Some("lab-local"),
            &patch.to_string(),
            &["resources".to_string()],
        )
        .expect("replace resources");

        let runner = crate::load("lab-local").expect("reload runner");
        let extensions = runner.resources["dev_sync"]["extensions"]
            .as_array()
            .expect("extensions array");
        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0]["source_path"], "/newer/nodejs");
    });
}

#[test]
fn extension_only_dev_sync_plan_does_not_refresh_homeboy_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{
                "id": "lab-local",
                "kind": "local",
                "workspace_root": "/runner/ws",
                "homeboy_path": "/runner/bin/homeboy"
            }"#,
            false,
        )
        .expect("create runner");
        let dir = tempfile::tempdir().expect("extension source");
        std::fs::write(dir.path().join("nodejs.json"), r#"{"id":"nodejs"}"#).expect("manifest");

        let options = RunnerDevSyncOptions {
            runner_id: "lab-local".to_string(),
            homeboy_source: None,
            homeboy_binary: None,
            extensions: vec![format!("nodejs={}", dir.path().display())],
            reconnect: false,
            dry_run: true,
        };
        let plan = plan_runner_dev_sync(&options).expect("plan dev-sync");

        assert!(!should_sync_homeboy_binary(&options));
        assert_eq!(plan.local_binary, None);
        assert_eq!(plan.remote_binary, None);
        assert!(plan.followup_commands.is_empty());
        assert_eq!(plan.extensions.len(), 1);
        assert_eq!(plan.extensions[0].id, "nodejs");
    });
}

#[test]
fn extension_only_dev_sync_scrubs_dev_binary_env() {
    let mut env = std::collections::HashMap::new();
    env.insert(
        "PATH".to_string(),
        "/runner/ws/_homeboy_binaries/dev/darwin:/usr/local/bin:/usr/bin".to_string(),
    );
    env.insert(
        "HOMEBOY_COMMAND".to_string(),
        "/runner/ws/_homeboy_binaries/dev/darwin/homeboy".to_string(),
    );
    env.insert("KEEP".to_string(), "yes".to_string());

    let scrubbed = installed_homeboy_env(
        &env,
        Some("/runner/ws/_homeboy_binaries/dev/darwin/homeboy"),
    );

    assert_eq!(scrubbed.get("HOMEBOY_COMMAND"), None);
    assert_eq!(
        scrubbed.get("PATH").map(String::as_str),
        Some("/usr/local/bin:/usr/bin")
    );
    assert_eq!(scrubbed.get("KEEP").map(String::as_str), Some("yes"));
}

#[test]
fn dev_sync_without_extensions_still_refreshes_homeboy_binary() {
    let options = RunnerDevSyncOptions {
        runner_id: "lab".to_string(),
        homeboy_source: None,
        homeboy_binary: None,
        extensions: Vec::new(),
        reconnect: false,
        dry_run: true,
    };

    assert!(should_sync_homeboy_binary(&options));
    assert!(!dev_sync_next_actions("lab", &options).is_empty());
}

#[test]
fn ssh_dev_sync_rejects_darwin_binary_before_upload() {
    let dir = tempfile::tempdir().expect("binary dir");
    let binary = dir.path().join("homeboy");
    std::fs::write(&binary, [0xcf, 0xfa, 0xed, 0xfe]).expect("write macho binary");
    let runner = super::super::super::Runner {
        id: "homeboy-lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some("lab-server".to_string()),
        workspace_root: Some("/home/chubes/Developer".to_string()),
        settings: Default::default(),
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: Default::default(),
    };

    let err =
        validate_dev_sync_binary_for_runner(&runner, &binary).expect_err("darwin binary rejected");

    assert!(err.message.contains("Darwin/Mach-O"));
    let tried = err.details["tried"].as_array().expect("tried remediation");
    assert!(tried.iter().any(|hint| hint.as_str().is_some_and(|hint| {
        hint.contains("runner refresh-homeboy") && hint.contains("--ref main --reconnect")
    })));
}

#[test]
fn ssh_source_snapshot_plan_builds_natively_without_cross_compilation() {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(
        source.path().join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .expect("manifest");
    std::fs::create_dir_all(source.path().join("target")).expect("target");
    std::fs::write(source.path().join("target/local"), "controller binary").expect("target output");
    let snapshot = build_runner_source_snapshot(source.path(), "/runner/ws").expect("snapshot");
    let script = source_snapshot_build_script(&snapshot);
    let archive = std::process::Command::new("tar")
        .args(["-tf"])
        .arg(snapshot.archive.path())
        .output()
        .expect("list snapshot archive");

    assert!(snapshot
        .remote_archive
        .starts_with("/runner/ws/_homeboy_binaries/dev-source/"));
    assert_eq!(
        snapshot.build_slot,
        format!(
            "/runner/ws/_homeboy_binaries/dev/{}",
            &snapshot.sha256[..16]
        )
    );
    assert!(script.contains("cargo build --release --bin homeboy"));
    assert!(script.contains("runner_native_build_not_elf"));
    assert!(!script.contains(source.path().to_str().expect("utf8 source")));
    assert!(!String::from_utf8_lossy(&archive.stdout).contains("target/local"));
}

#[test]
fn ssh_source_snapshot_requires_matching_source_binary_and_slot_identity() {
    let snapshot = PreparedRunnerSourceSnapshot {
        archive: tempfile::NamedTempFile::new().expect("archive"),
        sha256: "a".repeat(64),
        size_bytes: 1,
        remote_archive: "/runner/ws/_homeboy_binaries/dev-source/aaaaaaaaaaaaaaaa.tar".to_string(),
        build_slot: "/runner/ws/_homeboy_binaries/dev/aaaaaaaaaaaaaaaa".to_string(),
    };
    let binary = "b".repeat(64);
    let stdout = format!(
        "HOMEBOY_DEV_SOURCE_SHA256={}\nHOMEBOY_DEV_BINARY_SHA256={binary}\nHOMEBOY_DEV_BINARY_PATH={}/homeboy\n",
        snapshot.sha256, snapshot.build_slot
    );
    assert_eq!(
        verify_source_snapshot_build(&snapshot, &stdout).expect("verified"),
        (format!("{}/homeboy", snapshot.build_slot), binary)
    );
    let error = verify_source_snapshot_build(&snapshot, &stdout.replace('a', "c"))
        .expect_err("mismatched source rejected");
    assert!(error.message.contains("sealed source snapshot"));
}

#[test]
fn local_dev_sync_allows_darwin_binary() {
    let dir = tempfile::tempdir().expect("binary dir");
    let binary = dir.path().join("homeboy");
    std::fs::write(&binary, [0xcf, 0xfa, 0xed, 0xfe]).expect("write macho binary");
    let runner = super::super::super::Runner {
        id: "lab-local".to_string(),
        kind: RunnerKind::Local,
        server_id: None,
        workspace_root: Some("/tmp/homeboy".to_string()),
        settings: Default::default(),
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: Default::default(),
    };

    validate_dev_sync_binary_for_runner(&runner, &binary).expect("local runner accepts binary");
}

#[test]
fn extension_overlay_lifecycle_uses_ttl_cleanup_policy() {
    let lifecycle = super::super::super::extension_materialization::dev_extension_lifecycle(
        "lab",
        "/runner/ws/dev/rust/hash",
        "rust",
    );

    assert_eq!(lifecycle.owner, "runner.dev_sync.extension_overlay");
    assert_eq!(lifecycle.ttl.as_deref(), Some("P7D"));
    assert_eq!(
        lifecycle.cleanup_policy,
        homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteAfterTtl
    );
    assert_eq!(
        lifecycle.status,
        homeboy_core::resource_lifecycle_index::ResourceLifecycleResourceStatus::Active
    );
}

#[test]
fn refresh_patch_updates_the_selected_binary_and_control_plane_environment() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{
                "id": "lab-local",
                "kind": "local",
                "workspace_root": "/runner/ws",
                "homeboy_path": "/old/homeboy",
                "resources": {
                    "dev_sync": {"schema":"homeboy/runner-dev-sync/v1"},
                    "keep": {"enabled": true}
                }
            }"#,
            false,
        )
        .expect("create runner");

        let patch =
            refreshed_runner_patch("lab-local", "/runner/ws/homeboy").expect("build refresh patch");

        assert_eq!(patch["homeboy_path"], "/runner/ws/homeboy");
        assert_eq!(patch["env"]["HOMEBOY_COMMAND"], "/runner/ws/homeboy");
        assert!(
            patch["env"]["HOMEBOY_COMMAND"] == "/runner/ws/homeboy",
            "refresh updates env to pin daemon startup and queued jobs to the selected binary"
        );
        assert!(patch["env"]["HOMEBOY_DAEMON_STATE_DIR"].is_null());
    });
}

#[test]
fn ssh_bootstrap_success_promotes_verified_exact_sha_with_provenance() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let plan = ssh_bootstrap_plan();
        let result = ssh_bootstrap_promote_with(
            &plan,
            || Ok(verified_bootstrap_output("abc123")),
            |path, _| {
                let lease = acquire_runner_binary_promotion("lab-local", "abc123")?;
                promote_verified_runner_binary(&lease, "lab-local", path)
                    .map(|fields| (fields, None))
            },
        )
        .expect("verified bootstrap promotes");
        assert_eq!(result.source_sha.as_deref(), Some("abc123"));
        assert_eq!(result.identity["data"]["git_commit"], "abc123");
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/verified/homeboy")
        );
    });
}

#[test]
fn controller_binary_selection_reports_fresh_main_control_plane_fields() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");

        assert_eq!(
            {
                let lease = acquire_runner_binary_promotion("lab-local", "verified")
                    .expect("promotion lease");
                promote_verified_runner_binary(&lease, "lab-local", "/verified/homeboy")
            }
            .expect("persist controller selection"),
            ["env", "homeboy_path"]
        );
        assert_eq!(
            {
                let lease = acquire_runner_binary_promotion("lab-local", "verified")
                    .expect("promotion lease");
                promote_verified_runner_binary(&lease, "lab-local", "/verified/homeboy")
            }
                .expect("repeat controller selection"),
            ["env", "homeboy_path"],
            "promotion always declares the control-plane environment and selected executable it normalizes"
        );
        assert_eq!(
            crate::load("lab-local")
                .expect("reload controller registry")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/verified/homeboy")
        );
    });
}

#[test]
fn promotion_repairs_an_acknowledged_but_stale_configured_executable() {
    let configured_path = std::cell::RefCell::new(Some("/old/homeboy".to_string()));
    let writes = std::cell::RefCell::new(0);

    let updated_fields = promote_verified_runner_binary_with(
        || Ok(()),
        "lab",
        "/selected/homeboy",
        |_, selected| {
            *writes.borrow_mut() += 1;
            // Reproduce the registry split: the first merge reports success,
            // but the configured executable remains old until the repair write.
            if *writes.borrow() == 2 {
                configured_path.replace(Some(selected.to_string()));
            }
            Ok(vec!["homeboy_path".to_string()])
        },
        |_| Ok(configured_path.borrow().clone()),
    )
    .expect("promotion repairs stale configured executable before reconnect");

    assert_eq!(*writes.borrow(), 2);
    assert_eq!(
        configured_path.borrow().as_deref(),
        Some("/selected/homeboy")
    );
    assert_eq!(updated_fields, ["homeboy_path", "homeboy_path"]);
}

#[test]
fn promotion_repair_does_not_write_after_its_lease_loses_authority() {
    let configured_path = std::cell::RefCell::new(Some("/old/homeboy".to_string()));
    let writes = std::cell::RefCell::new(0);

    let error = promote_verified_runner_binary_with(
        || {
            if *writes.borrow() == 1 {
                return Err(Error::validation_invalid_argument(
                    "runtime_generation",
                    "promotion lease no longer owns the selected generation",
                    Some("new-generation".to_string()),
                    None,
                ));
            }
            Ok(())
        },
        "lab",
        "/old-selection/homeboy",
        |_, _| {
            *writes.borrow_mut() += 1;
            Ok(vec!["homeboy_path".to_string()])
        },
        |_| Ok(configured_path.borrow().clone()),
    )
    .expect_err("a stale promotion must not repair after losing its lease");

    assert_eq!(error.details["field"], "runtime_generation");
    assert_eq!(*writes.borrow(), 1, "only the acknowledged write occurred");
    assert_eq!(
        configured_path.borrow().as_deref(),
        Some("/old/homeboy"),
        "the stale request did not regain selection"
    );
}

#[test]
fn promotion_failure_names_the_exact_selected_executable_mutation() {
    let error = promote_verified_runner_binary_with(
        || Ok(()),
        "lab",
        "/selected/homeboy",
        |_, _| Ok(vec!["homeboy_path".to_string()]),
        |_| Ok(Some("/old/homeboy".to_string())),
    )
    .expect_err("an unobservable selection must not proceed to reconnect");

    assert_eq!(error.details["field"], "homeboy_path");
    let recovery = error.details["tried"][0]
        .as_str()
        .expect("exact repair command");
    assert!(recovery.contains("--select /selected/homeboy --reconnect"));
}

#[test]
fn equivalent_refresh_waiter_reloads_the_owner_selection_after_promotion_handoff() {
    test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().expect("fixture");
        let selected = fixture.path().join("homeboy");
        std::fs::write(
            &selected,
            "#!/bin/sh\nprintf '%s\\n' 'homeboy 0.1.0+0123456789ab'\n",
        )
        .expect("write selected binary");
        assert!(Command::new("chmod")
            .args(["0755", selected.to_str().expect("binary path")])
            .status()
            .expect("make binary executable")
            .success());
        crate::create(
            r#"{"id":"lab","kind":"local","homeboy_path":"/before"}"#,
            false,
        )
        .expect("create runner");

        let owner = acquire_runner_binary_promotion("lab", "0123456789ab").expect("owner lease");
        let (queued_tx, queued_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _lease = acquire_runner_binary_promotion_with("lab", "0123456789ab", |event| {
                queued_tx.send(event).expect("report queued owner")
            })?;
            let status = crate::status("lab")?;
            refresh_promotion_authorities("lab", &status)?;
            crate::load("lab")
        });
        let event = queued_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("waiter queues behind the owner");
        assert_eq!(event.target, "lab");
        assert_eq!(event.owner_operation, "runner binary promotion");
        assert_eq!(event.owner_pid, std::process::id());
        promote_verified_runner_binary(&owner, "lab", selected.to_str().expect("selected path"))
            .expect("owner selects candidate");
        drop(owner);

        let reloaded = waiter
            .join()
            .expect("waiter exits")
            .expect("waiter reloads promotion authorities");
        assert_eq!(reloaded.settings.homeboy_path.as_deref(), selected.to_str());
    });
}

#[test]
fn divergent_refresh_candidate_is_rejected_while_owner_keeps_selection() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","homeboy_path":"/stable"}"#,
            false,
        )
        .expect("create runner");
        let owner = acquire_runner_binary_promotion("lab", "newer").expect("owner lease");

        let error = std::thread::spawn(|| {
            acquire_runner_binary_promotion_with("lab", "diverged", |_| {
                panic!("divergent candidate must not queue")
            })
        })
        .join()
        .expect("divergent contender exits")
        .expect_err("divergent candidate remains fail-closed");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::RuntimePromotionContended
        );
        assert_eq!(error.details["holder_compatibility_key"], "newer");
        assert_eq!(
            crate::load("lab")
                .expect("reload runner")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable")
        );
        drop(owner);
    });
}

#[test]
fn strict_ancestor_refresh_candidate_is_rejected_while_owner_keeps_selection() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","homeboy_path":"/stable"}"#,
            false,
        )
        .expect("create runner");
        let owner = acquire_runner_binary_promotion("lab", "descendant").expect("owner lease");

        let error = std::thread::spawn(|| {
            acquire_runner_binary_promotion_with("lab", "ancestor", |_| {
                panic!("strict ancestor candidate must not queue")
            })
        })
        .join()
        .expect("strict ancestor contender exits")
        .expect_err("strict ancestor candidate remains fail-closed");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::RuntimePromotionContended
        );
        assert_eq!(error.details["holder_compatibility_key"], "descendant");
        assert_eq!(
            crate::load("lab")
                .expect("reload runner")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable")
        );
        drop(owner);
    });
}

#[test]
fn verified_selection_persists_on_controller_and_reports_reconnect_required() {
    test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().expect("fixture");
        let binary = fixture.path().join("homeboy");
        let commit = homeboy_product_identity::build_identity()
            .git_commit
            .unwrap_or_else(|| "exact-remote-sha".to_string());
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"data\":{{\"git_commit\":\"{commit}\",\"git_dirty\":false}}}}'\n"
            ),
        )
        .expect("write selected binary");
        let status = Command::new("chmod")
            .args(["0755", binary.to_str().expect("binary path")])
            .status()
            .expect("make selected binary executable");
        assert!(status.success());
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old/homeboy"}"#,
            false,
        )
        .expect("runner");
        let options = HomeboyBinaryRefreshOptions {
            runner_id: "lab-local".to_string(),
            mode: HomeboyBinaryRefreshMode::Select {
                binary_path: binary.display().to_string(),
            },
            source: None,
            git_ref: None,
            target_dir: None,
            reconnect: false,
            force: false,
            allow_downgrade: true,
            dry_run: false,
        };

        let (selected, exit_code) = refresh_homeboy_binary(options.clone()).expect("selection");
        assert_eq!(exit_code, 0);
        assert_eq!(selected.updated_fields, ["env", "homeboy_path"]);
        assert_eq!(selected.selected_binary_path, binary.display().to_string());
        assert!(!selected.daemon_refreshed);
        assert!(selected.reconnect_required);
        assert!(selected.next_actions.is_empty());
        assert_eq!(
            crate::load("lab-local")
                .expect("reload controller registry")
                .settings
                .homeboy_path
                .as_deref(),
            binary.to_str()
        );

        let (repeated, exit_code) = refresh_homeboy_binary(options).expect("repeat selection");
        assert_eq!(exit_code, 0);
        assert_eq!(repeated.updated_fields, ["env", "homeboy_path"]);
        assert!(!repeated.daemon_refreshed);
        assert!(repeated.reconnect_required);
        assert!(repeated.next_actions.is_empty());
    });
}

#[test]
fn verified_materialized_refresh_defers_controller_materialization_to_exact_continuation() {
    let identity = serde_json::json!({
        "data": {
            "display": "homeboy 1.2.3+exact-remote-sha",
            "git_commit": "exact-remote-sha"
        }
    });
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: Some("https://example.test/Extra-Chill/homeboy.git".to_string()),
        git_ref: Some("candidate".to_string()),
        target_dir: Some("/runner/homeboy".to_string()),
        binary_path: "/runner/homeboy".to_string(),
        script: "runner-only materialization".to_string(),
        reconnect: true,
        followup_commands: Vec::new(),
    };

    let actions = controller_continuation_actions(&plan, &identity).expect("continuation");
    assert_eq!(actions.len(), 1);
    let action = &actions[0];
    assert_eq!(action.commit, "exact-remote-sha");
    assert_eq!(
        action.source,
        "https://example.test/Extra-Chill/homeboy.git"
    );
    assert_eq!(
        action.command[0..3],
        ["homeboy", "runtime", "materialize-controller"]
    );
    assert!(action.command.iter().any(|arg| arg == "exact-remote-sha"));
    assert!(!action.command.iter().any(|arg| arg == "refresh-homeboy"));
    assert!(action.invocation.is_empty());
    assert!(!action
        .invocation
        .iter()
        .any(|arg| arg.contains("refresh-homeboy")));

    let select = HomeboyBinaryRefreshPlan {
        mode: "select".to_string(),
        source: None,
        ..plan
    };
    assert!(controller_continuation_actions(&select, &identity)
        .expect("select continuation")
        .is_empty());
}

#[test]
fn blocked_connect_preserves_successful_promotion_with_one_continuation() {
    test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().expect("fixture");
        let binary = fixture.path().join("homeboy");
        let commit = homeboy_product_identity::build_identity()
            .git_commit
            .unwrap_or_else(|| "exact-remote-sha".to_string());
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"data\":{{\"git_commit\":\"{commit}\",\"git_dirty\":false}}}}'\n"
            ),
        )
        .expect("write selected binary");
        assert!(Command::new("chmod")
            .args(["0755", binary.to_str().expect("binary path")])
            .status()
            .expect("make selected binary executable")
            .success());
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old/homeboy"}"#,
            false,
        )
        .expect("runner");

        let (output, exit_code) = refresh_homeboy_binary(HomeboyBinaryRefreshOptions {
            runner_id: "lab-local".to_string(),
            mode: HomeboyBinaryRefreshMode::Select {
                binary_path: binary.display().to_string(),
            },
            source: None,
            git_ref: None,
            target_dir: None,
            reconnect: true,
            force: false,
            allow_downgrade: true,
            dry_run: false,
        })
        .expect("blocked connect is a structured refresh result");

        assert_eq!(exit_code, 1);
        assert_eq!(output.updated_fields, ["env", "homeboy_path"]);
        assert_eq!(output.selected_binary_path, binary.display().to_string());
        assert_eq!(output.plan.mode, "select");
        assert_eq!(output.plan.source, None);
        assert_eq!(output.plan.git_ref, None);
        assert_eq!(output.plan.target_dir, None);
        assert!(!output.daemon_refreshed);
        assert!(output.reconnect_required);
        assert_eq!(
            output
                .phase_summary
                .iter()
                .map(|phase| (phase.name, phase.status))
                .collect::<Vec<_>>(),
            vec![
                ("select", "succeeded"),
                ("identity_verification", "succeeded"),
                ("bootstrap_promotion", "succeeded"),
                ("configuration_promotion", "succeeded"),
                ("reconnect_transport", "failed"),
                ("daemon_identity_verification", "succeeded"),
                ("admission_readiness", "failed"),
            ]
        );
        let readiness = output.readiness.expect("blocked readiness");
        assert_eq!(readiness.state, HomeboyRefreshReadinessState::Blocked);
        assert_eq!(
            readiness.continuation.as_deref(),
            Some("homeboy runner connect lab-local")
        );
        assert_eq!(
            output.followup_commands,
            ["homeboy runner connect lab-local"]
        );
        let failure = output.failure.expect("typed reconnect partial failure");
        assert_eq!(
            failure.recovery_actions[0].command,
            ["homeboy", "runner", "connect", "lab-local"]
        );
        assert!(failure
            .verification
            .as_deref()
            .expect("reconnect verification")
            .contains("promoted configured binary remains selected"));
        assert!(output.bootstrap_provenance.is_some());
        assert_eq!(
            crate::load("lab-local")
                .expect("reload controller registry")
                .settings
                .homeboy_path
                .as_deref(),
            binary.to_str()
        );
    });
}

#[test]
fn stale_session_refresh_blocker_starts_the_newly_selected_binary() {
    test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().expect("fixture");
        let second_binary = fixture.path().join("second-homeboy");
        let commit = homeboy_product_identity::build_identity()
            .git_commit
            .unwrap_or_else(|| "exact-remote-sha".to_string());
        std::fs::write(
            &second_binary,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"data\":{{\"git_commit\":\"{commit}\",\"git_dirty\":false}}}}'\n"
            ),
        )
        .expect("write selected binary");
        assert!(Command::new("chmod")
            .args(["0755", second_binary.to_str().expect("binary path")])
            .status()
            .expect("make selected binary executable")
            .success());
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old/homeboy"}"#,
            false,
        )
        .expect("runner");
        let refresh = |binary: &Path| {
            refresh_homeboy_binary(HomeboyBinaryRefreshOptions {
                runner_id: "lab-local".to_string(),
                mode: HomeboyBinaryRefreshMode::Select {
                    binary_path: binary.display().to_string(),
                },
                source: None,
                git_ref: None,
                target_dir: None,
                reconnect: true,
                force: false,
                allow_downgrade: true,
                dry_run: false,
            })
        };

        let hostname = String::from_utf8(
            Command::new("hostname")
                .output()
                .expect("read hostname")
                .stdout,
        )
        .expect("hostname is UTF-8")
        .trim()
        .to_string();
        let controller_id = format!("{hostname}-uid-{}", unsafe { libc::geteuid() });
        let session = RunnerSession {
            runner_id: "lab-local".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some("lab-local".to_string()),
            controller_id: Some(controller_id.clone()),
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:7421".to_string()),
            local_port: Some(7421),
            local_url: Some("http://127.0.0.1:7421".to_string()),
            tunnel_pid: None,
            tunnel_process_start_identity: None,
            proxy_forward: None,
            remote_daemon_pid: Some(2),
            remote_daemon_lease_id: Some("lease-existing".to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some(format!("homeboy test+{commit}")),
            connected_at: "2026-01-01T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        };
        let session_path =
            homeboy_core::paths::runner_controller_session_file("lab-local", &controller_id)
                .expect("session path");
        std::fs::create_dir_all(session_path.parent().expect("session directory"))
            .expect("create session directory");
        std::fs::write(
            session_path,
            serde_json::to_vec(&session).expect("serialize connected session"),
        )
        .expect("persist connected session");
        assert!(crate::connection::recorded_session("lab-local")
            .expect("read connected session")
            .is_some());

        let (output, exit_code) = refresh(&second_binary).expect("connected refresh result");

        assert_eq!(exit_code, 1);
        assert_eq!(output.updated_fields, ["env", "homeboy_path"]);
        assert_eq!(
            output.selected_binary_path,
            second_binary.display().to_string()
        );
        assert!(!output.daemon_refreshed);
        assert!(output.reconnect_required);
        assert_eq!(
            output
                .phase_summary
                .iter()
                .map(|phase| (phase.name, phase.status))
                .collect::<Vec<_>>(),
            vec![
                ("select", "succeeded"),
                ("identity_verification", "succeeded"),
                ("bootstrap_promotion", "succeeded"),
                ("configuration_promotion", "succeeded"),
                ("reconnect_transport", "failed"),
                ("daemon_identity_verification", "succeeded"),
                ("admission_readiness", "failed"),
            ]
        );
        assert_eq!(
            output.followup_commands,
            ["homeboy runner connect lab-local"]
        );
        assert_eq!(
            crate::load("lab-local")
                .expect("reload controller registry")
                .settings
                .homeboy_path
                .as_deref(),
            second_binary.to_str()
        );
    });
}

#[test]
fn select_without_source_rejects_implicit_downgrade_before_selection_or_reconnect() {
    test_support::with_isolated_home(|_| {
        let controller_commit = homeboy_product_identity::build_identity()
            .git_commit
            .expect("test build has an immutable controller commit");
        let fixture = tempfile::tempdir().expect("fixture");
        let binary = fixture.path().join("older-homeboy");
        let older = "0000000000000000000000000000000000000000";
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"data\":{{\"git_commit\":\"{older}\",\"git_dirty\":false}}}}'\n"
            ),
        )
        .expect("write selected binary");
        assert!(Command::new("chmod")
            .args(["0755", binary.to_str().expect("binary path")])
            .status()
            .expect("make selected binary executable")
            .success());
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old/homeboy"}"#,
            false,
        )
        .expect("runner");
        let options = HomeboyBinaryRefreshOptions {
            runner_id: "lab-local".to_string(),
            mode: HomeboyBinaryRefreshMode::Select {
                binary_path: binary.display().to_string(),
            },
            source: None,
            git_ref: Some("rollback-request".to_string()),
            target_dir: None,
            reconnect: true,
            force: false,
            allow_downgrade: false,
            dry_run: false,
        };

        let (rejected, exit_code) =
            refresh_homeboy_binary(options.clone()).expect("rejection output");
        assert_eq!(exit_code, 1);
        assert!(rejected
            .failure
            .expect("failure")
            .verification
            .unwrap()
            .contains("allow-downgrade"));
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/old/homeboy")
        );

        let (rolled_back, exit_code) = refresh_homeboy_binary(HomeboyBinaryRefreshOptions {
            allow_downgrade: true,
            reconnect: false,
            ..options
        })
        .expect("explicit rollback");
        assert_eq!(exit_code, 0);
        let rollback = rolled_back.rollback.expect("structured rollback evidence");
        assert!(rollback
            .unproven
            .iter()
            .any(|authority| authority.contains(&controller_commit)));
        assert!(rollback.previous.is_empty());
        assert_eq!(rollback.requested, None, "select mode has no requested ref");
        assert_eq!(rollback.resolved, older);
        assert_eq!(rollback.selected, older);
    });
}

#[test]
fn contending_refreshes_cannot_let_an_old_materialized_request_replace_new_selection() {
    test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().expect("git fixture");
        for args in [
            vec!["init", "--quiet"],
            vec!["config", "user.email", "homeboy@example.test"],
            vec!["config", "user.name", "Homeboy Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(fixture.path())
                .status()
                .expect("git")
                .success());
        }
        std::fs::write(fixture.path().join("release"), "old\n").expect("old");
        for args in [vec!["add", "."], vec!["commit", "-m", "old"]] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(fixture.path())
                .status()
                .expect("commit old")
                .success());
        }
        let revision = || {
            String::from_utf8(
                Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(fixture.path())
                    .output()
                    .expect("revision")
                    .stdout,
            )
            .expect("utf8")
            .trim()
            .to_string()
        };
        let old = revision();
        std::fs::write(fixture.path().join("release"), "new\n").expect("new");
        assert!(Command::new("git")
            .args(["commit", "-am", "new"])
            .current_dir(fixture.path())
            .status()
            .expect("commit new")
            .success());
        let new = revision();
        let marker = fixture.path().join("old-materialized");
        let old_binary = fixture.path().join("old-homeboy");
        let new_binary = fixture.path().join("new-homeboy");
        std::fs::write(
            &old_binary,
            format!(
                "#!/bin/sh\ntouch {}\nsleep 1\nprintf '%s\\n' '{{\"data\":{{\"git_commit\":\"{old}\",\"git_dirty\":false}}}}'\n",
                marker.display()
            ),
        )
        .expect("old binary");
        std::fs::write(
            &new_binary,
            format!("#!/bin/sh\nprintf '%s\\n' '{{\"data\":{{\"git_commit\":\"{new}\",\"git_dirty\":false}}}}'\n"),
        )
        .expect("new binary");
        for binary in [&old_binary, &new_binary] {
            assert!(Command::new("chmod")
                .args(["0755", binary.to_str().expect("binary")])
                .status()
                .expect("chmod")
                .success());
        }
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/stable/homeboy"}"#,
            false,
        )
        .expect("runner");
        let old_options = HomeboyBinaryRefreshOptions {
            runner_id: "lab-local".to_string(),
            mode: HomeboyBinaryRefreshMode::Select {
                binary_path: old_binary.display().to_string(),
            },
            source: None,
            git_ref: Some("old".to_string()),
            target_dir: Some(fixture.path().display().to_string()),
            reconnect: false,
            force: false,
            allow_downgrade: false,
            dry_run: false,
        };
        let old_refresh = std::thread::spawn(move || refresh_homeboy_binary(old_options));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists(), "old request materialized before selection");
        let (new_output, new_code) = refresh_homeboy_binary(HomeboyBinaryRefreshOptions {
            runner_id: "lab-local".to_string(),
            mode: HomeboyBinaryRefreshMode::Select {
                binary_path: new_binary.display().to_string(),
            },
            source: None,
            git_ref: Some("new".to_string()),
            target_dir: Some(fixture.path().display().to_string()),
            reconnect: false,
            force: false,
            allow_downgrade: true,
            dry_run: false,
        })
        .expect("new refresh");
        assert_eq!(new_code, 0);
        let (old_output, old_code) = old_refresh
            .join()
            .expect("old refresh thread")
            .expect("old refresh");
        assert_eq!(old_code, 1);
        assert!(old_output.failure.is_some());
        assert!(!old_output.daemon_refreshed);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            new_binary.to_str()
        );
        assert!(!new_output.daemon_refreshed);
        assert!(crate::connection::recorded_session("lab-local")
            .expect("session")
            .is_none());
    });
}

#[test]
fn ssh_bootstrap_select_promotes_without_materialized_source_sha() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let mut plan = ssh_bootstrap_plan();
        plan.mode = "select".to_string();
        plan.source = None;
        plan.git_ref = None;
        plan.target_dir = None;
        let result = ssh_bootstrap_promote_with(
            &plan,
            || Ok(r#"{"data":{"git_commit":"abc123","git_dirty":false}}"#.to_string()),
            |path, _| {
                homeboy_core::config::with_config_lock(|| {
                    let patch = refreshed_runner_patch("lab-local", path)?;
                    match merge(Some("lab-local"), &patch.to_string(), &[])? {
                        MergeOutput::Single(result) => Ok((result.updated_fields, None)),
                        MergeOutput::Bulk(_) => Ok((Vec::new(), None)),
                    }
                })
            },
        )
        .expect("selected binary promotes");
        assert_eq!(result.source_sha, None);
        assert_eq!(result.identity["data"]["git_commit"], "abc123");
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/verified/homeboy")
        );
    });
}

#[test]
fn ssh_bootstrap_transport_failure_leaves_config_unchanged() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let result = ssh_bootstrap_promote_with(
            &ssh_bootstrap_plan(),
            || Err(Error::internal_io("transport failed".to_string(), None)),
            |_, _| panic!("must not promote"),
        );
        assert!(result.is_err());
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/old")
        );
    });
}

#[test]
fn ssh_bootstrap_identity_mismatch_leaves_config_unchanged() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let result = ssh_bootstrap_promote_with(
            &ssh_bootstrap_plan(),
            || {
                Ok("HOMEBOY_REFRESH_SOURCE_SHA=abc123\n{\"data\":{\"git_commit\":\"other\",\"git_dirty\":false}}".to_string())
            },
            |_, _| panic!("must not promote"),
        );
        assert!(result.is_err());
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/old")
        );
    });
}

#[test]
fn refresh_rotation_predicate_preserves_owned_generations_without_changing_force_semantics() {
    assert!(should_rotate_daemon_generation(false, true, false));
    assert!(should_rotate_daemon_generation(true, false, false));
    assert!(!should_rotate_daemon_generation(false, false, false));
    assert!(!should_rotate_daemon_generation(true, true, true));
}

#[test]
fn same_revision_rebuild_rotates_active_daemon_to_a_distinct_byte_generation() {
    let revision = "homeboy 1.0.0+same-revision";
    let old_hash = "a".repeat(64);
    let rebuilt_hash = "b".repeat(64);
    let old_generation = refreshed_generation_key(revision, Some(old_hash));
    let rebuilt_generation = refreshed_generation_key(revision, Some(rebuilt_hash));
    assert_ne!(old_generation, rebuilt_generation);

    let mut generations = crate::RollingGenerations::new(old_generation.clone(), "old-daemon");
    generations.admit_job("active-job");
    assert_eq!(
        generations.begin(rebuilt_generation.clone(), "rebuilt-daemon"),
        crate::RollingStart::Start
    );
    assert!(generations.activate(&rebuilt_generation));
    assert_eq!(generations.admission_owner, rebuilt_generation);
    assert_eq!(
        generations.job_owner("active-job"),
        Some(old_generation.as_str())
    );
}

#[test]
fn materialized_refresh_requires_an_immutable_binary_hash_and_path() {
    let plan = ssh_bootstrap_plan();
    let error = refreshed_binary_path(
        &plan,
        "HOMEBOY_REFRESH_SOURCE_SHA=abc123\n{\"data\":{\"git_commit\":\"abc123\"}}",
    )
    .expect_err("materialized refresh must bind the selected path to its bytes");
    assert!(error.message.contains("immutable binary path"));
}

#[test]
fn concurrent_runner_config_edit_survives_ssh_bootstrap_promotion() {
    test_support::with_isolated_home(|_| {
        crate::create(r#"{"id":"lab-local","kind":"local","homeboy_path":"/old","env":{"OLD":"1"},"resources":{"dev_sync":{"old":true}}}"#, false).expect("runner");
        let plan = ssh_bootstrap_plan();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            started_rx.recv().expect("executor started");
            crate::merge(
                Some("lab-local"),
                r#"{"env":{"NEW":"2"},"resources":{"dev_sync":{"new":true}}}"#,
                &[],
            )
            .expect("concurrent config edit");
            release_tx.send(()).expect("release executor");
        });
        let result = ssh_bootstrap_promote_with(
            &plan,
            || {
                started_tx.send(()).expect("notify writer");
                release_rx.recv().expect("writer completed");
                Ok(verified_bootstrap_output("abc123"))
            },
            |path, _| {
                homeboy_core::config::with_config_lock(|| {
                    let patch = refreshed_runner_patch("lab-local", path)?;
                    match merge(Some("lab-local"), &patch.to_string(), &[])? {
                        MergeOutput::Single(result) => Ok((result.updated_fields, None)),
                        MergeOutput::Bulk(_) => Ok((Vec::new(), None)),
                    }
                })
            },
        )
        .expect("promote");
        writer.join().expect("writer");
        let runner = crate::load("lab-local").expect("reload");
        assert_eq!(
            runner.settings.homeboy_path.as_deref(),
            Some("/verified/homeboy")
        );
        assert_eq!(runner.env.get("NEW").map(String::as_str), Some("2"));
        assert_eq!(runner.resources["dev_sync"]["new"], true);
        assert_eq!(result.updated_fields, vec!["env", "homeboy_path"]);
    });
}
