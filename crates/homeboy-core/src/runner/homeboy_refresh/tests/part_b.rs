#![cfg(test)]

use super::*;
use crate::runner::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
use crate::test_support;

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
fn refreshed_runner_env_prepends_selected_homeboy_dir_to_path() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(
            r#"{
                "id": "lab-local",
                "kind": "local",
                "workspace_root": "/runner/ws",
                "homeboy_path": "/old/homeboy",
                "env": {"PATH": "/usr/bin:/bin", "RUST_LOG": "info"}
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
        crate::runner::create(
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

        let runner = crate::runner::load("lab-local").expect("load runner");
        let dev_sync =
            updated_dev_sync_resource(runner.resources.get("dev_sync").cloned(), None, &[])
                .expect("reconcile dev-sync resource");
        let patch = serde_json::json!({ "resources": { "dev_sync": dev_sync } });

        crate::runner::merge(
            Some("lab-local"),
            &patch.to_string(),
            &["resources".to_string()],
        )
        .expect("replace resources");

        let runner = crate::runner::load("lab-local").expect("reload runner");
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
        crate::runner::create(
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
        crate::resource_lifecycle_index::ResourceCleanupPolicy::DeleteAfterTtl
    );
    assert_eq!(
        lifecycle.status,
        crate::resource_lifecycle_index::ResourceLifecycleResourceStatus::Active
    );
}

#[test]
fn refresh_patch_only_owns_homeboy_path() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(
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
        assert_eq!(
            patch,
            serde_json::json!({ "homeboy_path": "/runner/ws/homeboy" })
        );
    });
}

#[test]
fn ssh_bootstrap_success_promotes_verified_exact_sha_with_provenance() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let plan = ssh_bootstrap_plan();
        let result = ssh_bootstrap_promote_with(
            &plan,
            || Ok(verified_bootstrap_output("abc123")),
            |path| {
                crate::config::with_config_lock(|| {
                    let patch = refreshed_runner_patch("lab-local", path)?;
                    match merge(Some("lab-local"), &patch.to_string(), &[])? {
                        MergeOutput::Single(result) => Ok(result.updated_fields),
                        MergeOutput::Bulk(_) => Ok(Vec::new()),
                    }
                })
            },
        )
        .expect("verified bootstrap promotes");
        assert_eq!(result.source_sha.as_deref(), Some("abc123"));
        assert_eq!(result.identity["data"]["git_commit"], "abc123");
        assert_eq!(
            crate::runner::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/verified/homeboy")
        );
    });
}

#[test]
fn ssh_bootstrap_select_promotes_without_materialized_source_sha() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(
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
            |path| {
                crate::config::with_config_lock(|| {
                    let patch = refreshed_runner_patch("lab-local", path)?;
                    match merge(Some("lab-local"), &patch.to_string(), &[])? {
                        MergeOutput::Single(result) => Ok(result.updated_fields),
                        MergeOutput::Bulk(_) => Ok(Vec::new()),
                    }
                })
            },
        )
        .expect("selected binary promotes");
        assert_eq!(result.source_sha, None);
        assert_eq!(result.identity["data"]["git_commit"], "abc123");
        assert_eq!(
            crate::runner::load("lab-local")
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
        crate::runner::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let result = ssh_bootstrap_promote_with(
            &ssh_bootstrap_plan(),
            || Err(Error::internal_io("transport failed".to_string(), None)),
            |_| panic!("must not promote"),
        );
        assert!(result.is_err());
        assert_eq!(
            crate::runner::load("lab-local")
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
        crate::runner::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let result = ssh_bootstrap_promote_with(
            &ssh_bootstrap_plan(),
            || {
                Ok("HOMEBOY_REFRESH_SOURCE_SHA=abc123\n{\"data\":{\"git_commit\":\"other\",\"git_dirty\":false}}".to_string())
            },
            |_| panic!("must not promote"),
        );
        assert!(result.is_err());
        assert_eq!(
            crate::runner::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/old")
        );
    });
}

#[test]
fn concurrent_runner_config_edit_survives_ssh_bootstrap_promotion() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(r#"{"id":"lab-local","kind":"local","homeboy_path":"/old","env":{"OLD":"1"},"resources":{"dev_sync":{"old":true}}}"#, false).expect("runner");
        let plan = ssh_bootstrap_plan();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            started_rx.recv().expect("executor started");
            crate::runner::merge(
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
            |path| {
                crate::config::with_config_lock(|| {
                    let patch = refreshed_runner_patch("lab-local", path)?;
                    match merge(Some("lab-local"), &patch.to_string(), &[])? {
                        MergeOutput::Single(result) => Ok(result.updated_fields),
                        MergeOutput::Bulk(_) => Ok(Vec::new()),
                    }
                })
            },
        )
        .expect("promote");
        writer.join().expect("writer");
        let runner = crate::runner::load("lab-local").expect("reload");
        assert_eq!(
            runner.settings.homeboy_path.as_deref(),
            Some("/verified/homeboy")
        );
        assert_eq!(runner.env.get("NEW").map(String::as_str), Some("2"));
        assert_eq!(runner.resources["dev_sync"]["new"], true);
        assert_eq!(result.updated_fields, vec!["homeboy_path"]);
    });
}
