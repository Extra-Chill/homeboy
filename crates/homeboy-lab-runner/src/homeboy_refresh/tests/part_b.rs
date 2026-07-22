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
fn refreshed_runner_env_prepends_selected_homeboy_dir_to_path() {
    test_support::with_isolated_home(|_| {
        crate::create(
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
fn refresh_patch_only_owns_homeboy_path() {
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
        assert_eq!(
            patch,
            serde_json::json!({ "homeboy_path": "/runner/ws/homeboy" })
        );
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
                promote_verified_runner_binary("lab-local", path).map(|fields| (fields, None))
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
fn controller_binary_selection_is_idempotent() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");

        assert_eq!(
            promote_verified_runner_binary("lab-local", "/verified/homeboy")
                .expect("persist controller selection"),
            ["homeboy_path"]
        );
        assert!(
            promote_verified_runner_binary("lab-local", "/verified/homeboy")
                .expect("repeat controller selection")
                .is_empty()
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
        assert_eq!(selected.updated_fields, ["homeboy_path"]);
        assert_eq!(selected.selected_binary_path, binary.display().to_string());
        assert!(!selected.daemon_refreshed);
        assert!(selected.reconnect_required);
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
        assert!(repeated.updated_fields.is_empty());
        assert!(!repeated.daemon_refreshed);
        assert!(repeated.reconnect_required);
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
        assert_eq!(result.updated_fields, vec!["homeboy_path"]);
    });
}
