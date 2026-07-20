#![cfg(test)]

use super::*;
use crate::{
    RunnerActiveJobState, RunnerSessionState, RunnerStaleDaemonWarning, RunnerStatusReport,
};
use crate::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
use homeboy_core::test_support;

#[test]
fn routine_reconnect_refuses_to_interrupt_an_admitted_lab_offload() {
    let admission = active_admission("0b77251a-b6a7-42a6-91a3-e49ff5f57c16");
    let error = protect_active_jobs_before_reconnect("homeboy-lab", &[admission], false)
        .expect_err("routine reconnect must preserve the active cook");

    assert_eq!(
        error.details["active_job_ids"],
        serde_json::json!(["0b77251a-b6a7-42a6-91a3-e49ff5f57c16"])
    );
    assert!(error.message.contains("homeboy-lab"));
    assert!(error.message.contains("--force"));
    assert!(error.details["tried"][0]
        .as_str()
        .is_some_and(|command| command.contains("runner job logs homeboy-lab")));
}

#[test]
fn forced_reconnect_reports_the_jobs_it_will_interrupt() {
    let admission = active_admission("0b77251a-b6a7-42a6-91a3-e49ff5f57c16");
    let interrupted = protect_active_jobs_before_reconnect("homeboy-lab", &[admission], true)
        .expect("explicit force permits interruption");

    assert_eq!(
        interrupted,
        vec!["0b77251a-b6a7-42a6-91a3-e49ff5f57c16".to_string()]
    );
}

#[test]
fn deferred_reconnect_reports_selected_binary_and_exact_active_job_followups() {
    let job_id = "0b77251a-b6a7-42a6-91a3-e49ff5f57c16".to_string();
    let followups = active_job_followups("homeboy-lab", std::slice::from_ref(&job_id));

    let deferred = HomeboyReconnectDeferred {
        reason: "active_daemon_jobs",
        active_job_ids: vec![job_id.clone()],
        selected_binary_path: "/runner/homeboy".to_string(),
        followup_commands: followups.clone(),
    };

    assert_eq!(deferred.active_job_ids, vec![job_id]);
    assert_eq!(deferred.selected_binary_path, "/runner/homeboy");
    assert_eq!(
        followups,
        vec![
            "homeboy runner job logs homeboy-lab 0b77251a-b6a7-42a6-91a3-e49ff5f57c16 --follow",
            "homeboy runner refresh-homeboy homeboy-lab --reconnect",
        ]
    );
}

fn active_admission(job_id: &str) -> homeboy_core::api_jobs::ActiveRunnerJobSummary {
    homeboy_core::api_jobs::ActiveRunnerJobSummary {
        runner_id: "homeboy-lab".to_string(),
        job_id: job_id.to_string(),
        operation: "runner.admission".to_string(),
        source: "runner-daemon".to_string(),
        kind: "runner.admission".to_string(),
        status: homeboy_core::api_jobs::JobStatus::Queued,
        command: "homeboy generic-workload".to_string(),
        cwd: None,
        started_at_ms: 1,
        updated_at_ms: 1,
        elapsed_ms: 0,
        heartbeat_age_ms: 0,
        claim: Default::default(),
        claim_expires_in_ms: None,
        lifecycle: None,
        durable_run_id: None,
        stale_reason: None,
        lifecycle_state: Some("active".to_string()),
        retryable: Some(false),
        active_child_count: None,
        active_cell_count: None,
    }
}

#[test]
fn refresh_preserves_only_its_direct_controller_lease_for_orphan_recovery() {
    let session = RunnerSession {
        runner_id: "lab".to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some("lab".to_string()),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:7421".to_string()),
        local_port: Some(7421),
        local_url: Some("http://127.0.0.1:7421".to_string()),
        tunnel_pid: Some(1),
        remote_daemon_pid: Some(2),
        remote_daemon_lease_id: Some("lease-refresh".to_string()),
        homeboy_version: "test".to_string(),
        homeboy_build_identity: None,
        connected_at: "2026-01-01T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    };

    assert_eq!(
        refresh_owned_lease(session),
        Some("lease-refresh".to_string())
    );
}

fn refreshed_daemon_status(connected: bool, identity: Option<&str>) -> RunnerStatusReport {
    RunnerStatusReport {
        runner_id: "lab".to_string(),
        connected,
        state: if connected {
            RunnerSessionState::Connected
        } else {
            RunnerSessionState::Disconnected
        },
        session: identity.map(|homeboy_build_identity| RunnerSession {
            runner_id: "lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some(homeboy_build_identity.to_string()),
            connected_at: "2026-01-01T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }),
        stale_daemon: None,
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        stale_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::NotQueried,
        active_job_source: None,
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: String::new(),
    }
}

#[test]
fn refreshed_daemon_verification_accepts_the_post_start_health_window() {
    let not_ready = refreshed_daemon_status(false, Some("homeboy 0.1.0+06bbf46013cf"));
    let ready = refreshed_daemon_status(true, Some("homeboy 0.1.0+06bbf46013cf"));
    let mut statuses = [not_ready, ready].into_iter();
    let mut retries = 0;

    verify_refreshed_daemon_identity_with(
        "lab",
        "06bbf46013cf",
        || Ok(statuses.next().expect("post-start status probe")),
        || retries += 1,
    )
    .expect("the persisted connected session identifies the requested daemon commit");
    assert_eq!(retries, 1, "the initial tunnel health race is retried once");
}

#[test]
fn refreshed_daemon_verification_converges_after_exact_identity_refresh() {
    let mut stale = refreshed_daemon_status(true, Some("homeboy 0.294.0+oldcommit"));
    stale.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "lab",
        "0.294.0".to_string(),
        "0.294.0".to_string(),
        Some("homeboy 0.294.0+oldcommit".to_string()),
        Some("homeboy 0.294.0+19a41cd5102d".to_string()),
    ));
    let converged = refreshed_daemon_status(true, Some("homeboy 0.294.0+19a41cd5102d"));
    let mut statuses = [stale, converged].into_iter();
    let mut retries = 0;

    verify_refreshed_daemon_identity_with(
        "lab",
        "19a41cd5102d",
        || Ok(statuses.next().expect("refresh convergence status")),
        || retries += 1,
    )
    .expect("refresh converges once daemon and configured executable identities match");
    assert_eq!(retries, 1);
}

#[test]
fn refreshed_daemon_verification_rejects_commit_substring_mismatch() {
    let status = refreshed_daemon_status(true, Some("homeboy 0.1.0+x06bbf46013cf"));

    let error = verify_refreshed_daemon_status("lab", "06bbf46013cf", &status)
        .expect_err("the daemon commit component must match exactly");
    assert!(error.message.contains("expected commit `06bbf46013cf`"));
}

#[test]
fn refreshed_daemon_rollback_stops_restores_and_reconnects_the_previous_binary() {
    let operations = std::cell::RefCell::new(Vec::new());

    rollback_refreshed_daemon_with(
        Some("/stable/homeboy"),
        || {
            operations.borrow_mut().push("stop new daemon".to_string());
            Ok(())
        },
        |path| {
            operations
                .borrow_mut()
                .push(format!("restore {}", path.expect("previous binary")));
            Ok(())
        },
        |path| {
            operations
                .borrow_mut()
                .push(format!("reconnect {}", path.expect("previous binary")));
            Ok(())
        },
    )
    .expect("rollback converges on the previous binary");

    assert_eq!(
        operations.into_inner(),
        [
            "stop new daemon",
            "restore /stable/homeboy",
            "reconnect /stable/homeboy",
        ]
    );
}

#[test]
fn materialize_plan_uses_clean_runner_cache() {
    let options = HomeboyBinaryRefreshOptions {
        runner_id: "lab".to_string(),
        mode: HomeboyBinaryRefreshMode::Materialize,
        source: Some("https://example.test/homeboy.git".to_string()),
        git_ref: Some("fix/foo".to_string()),
        target_dir: Some("/runner/ws/homeboy-clean".to_string()),
        reconnect: false,
        force: false,
        dry_run: true,
    };
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: options.source.clone(),
        git_ref: options.git_ref.clone(),
        target_dir: options.target_dir.clone(),
        binary_path: "/runner/ws/homeboy-clean/target/release/homeboy".to_string(),
        script: materialize_script(
            "https://example.test/homeboy.git",
            "fix/foo",
            "/runner/ws/homeboy-clean",
            "/runner/ws/homeboy-clean/target/release/homeboy",
        ),
        reconnect: false,
        followup_commands: refresh_followups("lab", false),
    };

    assert!(plan.script.contains("git clone \"$source\" \"$dir\""));
    assert!(plan
        .script
        .contains("rev-parse --verify --quiet \"${requested}^{commit}\""));
    assert!(plan.script.contains("checkout --quiet --force --detach"));
    assert!(plan.script.contains("cargo build --release --bin homeboy"));
    assert_eq!(
        plan.binary_path,
        "/runner/ws/homeboy-clean/target/release/homeboy"
    );
}

#[test]
fn materialize_script_records_the_peeled_commit_for_tags_and_direct_commits() {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let source = fixture.path().join("source");
    let core = source.join("crates/homeboy-core");
    std::fs::create_dir_all(core.join("src")).expect("source directory");

    std::fs::write(
        source.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/homeboy-core\"]\n\n[package]\nname = \"homeboy\"\nversion = \"9.8.7\"\nedition = \"2021\"\n\n[[bin]]\nname = \"homeboy\"\npath = \"src/main.rs\"\n\n[dependencies]\nhomeboy-core = { path = \"crates/homeboy-core\" }\n",
    )
    .expect("write root manifest");
    std::fs::create_dir_all(source.join("src")).expect("root source directory");
    std::fs::write(
        source.join("src/main.rs"),
        "fn main() { println!(\"{{\\\"data\\\":{{\\\"git_commit\\\":\\\"{}\\\",\\\"git_dirty\\\":{}}}}}\", homeboy_core::git_commit(), homeboy_core::git_dirty()); }\n",
    )
    .expect("write root binary");
    std::fs::write(
        core.join("Cargo.toml"),
        "[package]\nname = \"homeboy-core\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n",
    )
    .expect("write core manifest");
    // The build-identity script lives in the product-identity crate at the
    // workspace root. After lab-runner was extracted into `crates/homeboy-lab-runner`,
    // CARGO_MANIFEST_DIR points at this crate (which has no build.rs), so resolve
    // the real generator from the workspace root instead.
    let build_identity_script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("crates/homeboy-product-identity/build.rs");
    std::fs::copy(&build_identity_script, core.join("build.rs"))
        .expect("copy core build identity script");
    // The generator emits HOMEBOY_PRODUCT_GIT_* (renamed from HOMEBOY_BUILD_GIT_*),
    // so the fixture's core crate consumes the current variable names.
    std::fs::write(
        core.join("src/lib.rs"),
        "pub fn git_commit() -> &'static str { env!(\"HOMEBOY_PRODUCT_GIT_COMMIT\") }\npub fn git_dirty() -> bool { env!(\"HOMEBOY_PRODUCT_GIT_DIRTY\") == \"true\" }\n",
    )
    .expect("write core build identity consumer");

    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "Homeboy Test"],
        vec!["config", "user.email", "homeboy@example.test"],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&source)
            .status()
            .expect("set up source fixture");
        assert!(status.success(), "source fixture setup succeeds");
    }
    std::fs::write(source.join("README.md"), "fixture\n").expect("write fixture");
    // Keep the materialized build tree clean: `cargo build` writes `target/` and
    // `Cargo.lock` into the clone, and the build-identity generator marks the
    // build dirty if `git status --porcelain` reports anything. Ignore those
    // build artifacts so the fixture's source-built binary reports a clean build.
    std::fs::write(source.join(".gitignore"), "/target\n/Cargo.lock\n").expect("write gitignore");
    for args in [vec!["add", "."], vec!["commit", "-m", "fixture"]] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&source)
            .status()
            .expect("commit source fixture");
        assert!(status.success(), "source fixture commit succeeds");
    }
    let commit = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&source)
            .output()
            .expect("read fixture commit")
            .stdout,
    )
    .expect("fixture commit is UTF-8")
    .trim()
    .to_string();
    for args in [
        vec!["tag", "-a", "annotated", "-m", "annotated fixture"],
        vec!["tag", "lightweight"],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&source)
            .status()
            .expect("tag source fixture");
        assert!(status.success(), "source fixture tag succeeds");
    }
    let annotated_object = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "annotated"])
            .current_dir(&source)
            .output()
            .expect("read annotated tag object")
            .stdout,
    )
    .expect("annotated tag object is UTF-8")
    .trim()
    .to_string();
    assert_ne!(annotated_object, commit, "fixture tag is annotated");

    for (index, git_ref) in ["annotated", "lightweight", commit.as_str()]
        .iter()
        .enumerate()
    {
        let target_dir = fixture.path().join(format!("build-{index}"));
        let binary_path = target_dir.join("target/release/homeboy");
        let script = materialize_script(
            source.to_str().expect("source path"),
            git_ref,
            target_dir.to_str().expect("target path"),
            binary_path.to_str().expect("binary path"),
        );
        let output = Command::new("bash")
            .args(["-c", &script])
            .output()
            .expect("run materialize script");
        assert!(
            output.status.success(),
            "materialize {git_ref} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("script output is UTF-8");
        assert_eq!(
            source_sha_from_output(&stdout).as_deref(),
            Some(commit.as_str())
        );
        verify_materialized_identity(
            &ssh_bootstrap_plan(),
            &stdout,
            &parse_identity(&stdout).expect("source-built binary identity"),
        )
        .expect("peeled source identity matches the nested-workspace build commit");
    }
}

#[test]
fn materialize_failure_preserves_compiler_diagnostics_and_active_binary() {
    test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().expect("fixture directory");
        let source = fixture.path().join("source");
        let workspace = fixture.path().join("workspace");
        let bin = fixture.path().join("bin");
        std::fs::create_dir_all(source.join("src")).expect("source directory");
        std::fs::create_dir_all(&workspace).expect("workspace directory");
        std::fs::create_dir_all(&bin).expect("tool directory");
        let cargo = bin.join("cargo");
        std::fs::write(
            &cargo,
            "#!/bin/sh\necho compiler_diagnostic_marker >&2\nexit 101\n",
        )
        .expect("fake cargo");
        let status = Command::new("chmod")
            .args(["0755", cargo.to_str().expect("cargo path")])
            .status()
            .expect("make fake cargo executable");
        assert!(status.success(), "fake cargo is executable");
        std::fs::write(
            source.join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");
        std::fs::write(
            source.join("src/main.rs"),
            "fn main() { compiler_diagnostic_marker }\n",
        )
        .expect("invalid source");
        for args in [
            vec!["init", "-b", "main"],
            vec!["add", "."],
            vec![
                "-c",
                "user.email=homeboy@example.test",
                "-c",
                "user.name=Homeboy Test",
                "commit",
                "-m",
                "fixture",
            ],
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .expect("run git");
            assert!(status.success(), "git fixture setup succeeds");
        }
        let source_sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&source)
                .output()
                .expect("read source SHA")
                .stdout,
        )
        .expect("source SHA is UTF-8")
        .trim()
        .to_string();
        crate::create(
            &format!(
                r#"{{"id":"lab-local","kind":"local","workspace_root":"{}","homeboy_path":"/active/homeboy","env":{{"PATH":"{}:{}"}}}}"#,
                workspace.display(),
                bin.display(),
                std::env::var("PATH").expect("PATH")
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = refresh_homeboy_binary(HomeboyBinaryRefreshOptions {
            runner_id: "lab-local".to_string(),
            mode: HomeboyBinaryRefreshMode::Materialize,
            source: Some(source.display().to_string()),
            git_ref: Some("main".to_string()),
            target_dir: Some(workspace.join("build").display().to_string()),
            reconnect: false,
            force: false,
            dry_run: false,
        })
        .expect("refresh returns diagnostics for compiler failure");

        assert_eq!(
            exit_code,
            101,
            "stdout: {}\nstderr: {}",
            output
                .failure
                .as_ref()
                .map(|failure| failure.stdout.as_str())
                .unwrap_or_default(),
            output
                .failure
                .as_ref()
                .map(|failure| failure.stderr.as_str())
                .unwrap_or_default()
        );
        let failure = output.failure.expect("failure evidence is preserved");
        assert_eq!(failure.exit_code, 101);
        assert_eq!(failure.source_sha.as_deref(), Some(source_sha.as_str()));
        assert!(failure
            .failed_command
            .starts_with(&["bash".to_string(), "-lc".to_string()]));
        assert!(failure
            .build_path
            .ends_with("/build/target/release/homeboy"));
        assert!(failure.stderr.contains("compiler_diagnostic_marker"));
        assert!(failure.capture.is_some());
        assert!(failure.execution_record.is_some());
        assert_eq!(
            crate::load("lab-local")
                .expect("reload runner")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/active/homeboy")
        );
    });
}

#[test]
fn select_plan_only_probes_requested_binary() {
    let script = identity_probe_script("/opt/homeboy/bin/homeboy");

    assert!(script.contains("binary='/opt/homeboy/bin/homeboy'"));
    assert!(script.contains("\"$binary\" self identity"));
    assert!(!script.contains("cargo build"));
}

#[test]
fn select_without_materialization_sha_promotes_the_verified_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let mut plan = ssh_bootstrap_plan();
        plan.mode = "select".to_string();
        plan.binary_path = "/selected/homeboy".to_string();

        let promoted = ssh_bootstrap_promote_with(
            &plan,
            || Ok(r#"{"data":{"git_commit":"abc123","git_dirty":false}}"#.to_string()),
            |path| {
                let patch = refreshed_runner_patch("lab-local", path)?;
                match merge(Some("lab-local"), &patch.to_string(), &[])? {
                    MergeOutput::Single(result) => Ok(result.updated_fields),
                    MergeOutput::Bulk(_) => Ok(Vec::new()),
                }
            },
        )
        .expect("select has no materialization SHA requirement");

        assert_eq!(promoted.source_sha, None);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/selected/homeboy")
        );
    });
}

#[test]
fn disconnect_failure_after_selection_restores_the_pre_refresh_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/stable/homeboy"}"#,
            false,
        )
        .expect("runner");
        let patch =
            refreshed_runner_patch("lab-local", "/selected/homeboy").expect("selection patch");
        merge(Some("lab-local"), &patch.to_string(), &[]).expect("select binary");

        let error = rollback_refresh_error::<()>(
            "lab-local",
            Some("/stable/homeboy"),
            Error::validation_invalid_argument(
                "disconnect",
                "request lease-bound daemon stop: tunnel unavailable",
                None,
                None,
            ),
        )
        .expect_err("disconnect failure rolls back selection");
        assert!(error.message.contains("lease-bound daemon stop"));

        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable/homeboy")
        );
    });
}

#[test]
fn reconnect_error_after_disconnect_restores_the_pre_refresh_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/stable/homeboy"}"#,
            false,
        )
        .expect("runner");
        let patch =
            refreshed_runner_patch("lab-local", "/selected/homeboy").expect("selection patch");
        merge(Some("lab-local"), &patch.to_string(), &[]).expect("select binary");

        let error = rollback_refresh_error::<()>(
            "lab-local",
            Some("/stable/homeboy"),
            Error::internal_io("reconnect transport failed".to_string(), None),
        )
        .expect_err("reconnect error rolls back selection");
        assert_eq!(error.details["error"], "reconnect transport failed");
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable/homeboy")
        );
    });
}

#[test]
fn reconnect_rollback_restores_only_its_own_selected_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/newer/homeboy"}"#,
            false,
        )
        .expect("runner");

        let restored = restore_runner_homeboy_path_if_selected(
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
        )
        .expect("compare and restore");

        assert!(!restored);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/newer/homeboy")
        );
    });
}

#[test]
fn reconnect_rollback_restores_its_own_selected_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/selected/homeboy"}"#,
            false,
        )
        .expect("runner");

        let restored = restore_runner_homeboy_path_if_selected(
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
        )
        .expect("compare and restore");

        assert!(restored);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable/homeboy")
        );
    });
}

#[test]
fn reconnect_failure_after_stop_restores_and_reconnects_before_returning() {
    let operations = std::cell::RefCell::new(Vec::new());

    let error = rollback_refresh_connect_error_with::<(), _, _>(
        Error::internal_io("selected daemon reconnect failed".to_string(), None),
        || {
            operations
                .borrow_mut()
                .push("restore old binary".to_string());
            Ok(())
        },
        || {
            operations
                .borrow_mut()
                .push("persist old-or-new authoritative lease".to_string());
            Ok(())
        },
    )
    .expect_err("the original reconnect failure remains visible after convergence");

    assert_eq!(error.details["error"], "selected daemon reconnect failed");
    assert_eq!(
        operations.into_inner(),
        [
            "restore old binary",
            "persist old-or-new authoritative lease"
        ],
        "a failed reconnect after the old session is removed must compensate before returning"
    );
}

#[test]
fn nonzero_reconnect_report_rollback_restores_the_pre_refresh_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/stable/homeboy"}"#,
            false,
        )
        .expect("runner");
        let patch =
            refreshed_runner_patch("lab-local", "/selected/homeboy").expect("selection patch");
        merge(Some("lab-local"), &patch.to_string(), &[]).expect("select binary");

        restore_runner_homeboy_path("lab-local", Some("/stable/homeboy"))
            .expect("rollback after nonzero reconnect report");

        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable/homeboy")
        );
    });
}

#[test]
fn rollback_failure_keeps_the_primary_refresh_error() {
    let error = rollback_refresh_error_with::<(), _>(
        Error::validation_invalid_argument("disconnect", "primary stop failure", None, None),
        || {
            Err(Error::internal_io(
                "rollback write failure".to_string(),
                None,
            ))
        },
    )
    .expect_err("rollback failure is surfaced with the primary failure");

    assert!(error.message.contains("primary stop failure"));
    assert!(error.message.contains("rollback write failure"));
    assert_eq!(
        error.details["rollback_error"]["details"]["error"],
        "rollback write failure"
    );
}

#[test]
fn default_target_dir_is_ref_scoped() {
    assert_eq!(
        default_target_dir("/runner/ws/", "origin/main"),
        "/runner/ws/_homeboy_binaries/homeboy-origin-main"
    );
}

#[test]
fn parse_identity_reads_final_pretty_json_after_command_output() {
    let identity = parse_identity(
        "HEAD is now at abc123 fix runner\n{\n  \"success\": true,\n  \"data\": {\n    \"version\": \"0.263.0\"\n  }\n}\n",
    )
    .expect("identity parses");

    assert_eq!(identity["data"]["version"], "0.263.0");
}

#[test]
fn disconnected_ssh_refresh_dispatches_the_existing_script_with_bounded_transport() {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: Some("https://example.test/homeboy.git".to_string()),
        git_ref: Some("accepted-sha".to_string()),
        target_dir: Some("/runner/homeboy".to_string()),
        binary_path: "/runner/homeboy/target/release/homeboy".to_string(),
        script: "managed clone fetch build select".to_string(),
        reconnect: true,
        followup_commands: Vec::new(),
    };

    let options = refresh_execution_options(
        &plan,
        vec!["bash".to_string(), "git".to_string(), "cargo".to_string()],
        true,
    );

    assert!(options.allow_diagnostic_ssh);
    assert_eq!(
        options.diagnostic_ssh_timeout,
        Some(DISCONNECTED_SSH_REFRESH_TIMEOUT)
    );
    assert_eq!(
        options.command,
        vec!["bash", "-lc", "managed clone fetch build select"]
    );
    assert_eq!(
        options
            .capability_preflight
            .expect("preflight")
            .required_commands,
        vec!["bash", "git", "cargo"]
    );
}

#[test]
fn connected_refresh_keeps_daemon_execution_options() {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "select".to_string(),
        source: None,
        git_ref: None,
        target_dir: None,
        binary_path: "/runner/homeboy".to_string(),
        script: "probe".to_string(),
        reconnect: false,
        followup_commands: Vec::new(),
    };

    let options = refresh_execution_options(&plan, vec!["bash".to_string()], false);

    assert!(!options.allow_diagnostic_ssh);
    assert_eq!(options.diagnostic_ssh_timeout, None);
}

#[test]
fn materialized_identity_must_match_the_resolved_ref_and_be_clean() {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: Some("source".to_string()),
        git_ref: Some("accepted-sha".to_string()),
        target_dir: Some("/runner/homeboy".to_string()),
        binary_path: "/runner/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    let wrong_identity = serde_json::json!({
        "data": { "git_commit": "badc0ffee", "git_dirty": false }
    });

    let error = verify_materialized_identity(
        &plan,
        "HOMEBOY_REFRESH_SOURCE_SHA=accepted-sha-123456\n",
        &wrong_identity,
    )
    .expect_err("a different built commit must not be selected");

    assert!(error.contains("does not match resolved ref"));
}

#[test]
fn materialized_identity_accepts_production_clean_envelope_without_dirty_metadata() {
    let plan = ssh_bootstrap_plan();
    let source_sha = "18915b824fdf";
    let identity = serde_json::json!({
        "success": true,
        "data": {
            "version": "0.284.1",
            "git_commit": source_sha,
            "display": "homeboy 0.284.1+18915b824fdf"
        }
    });

    verify_materialized_identity(
        &plan,
        &format!("HOMEBOY_REFRESH_SOURCE_SHA={source_sha}\n"),
        &identity,
    )
    .expect("production clean identity is accepted");
}

#[test]
fn materialized_identity_accepts_explicit_clean_state() {
    let plan = ssh_bootstrap_plan();
    let identity = serde_json::json!({
        "data": { "git_commit": "abc123", "git_dirty": false }
    });

    verify_materialized_identity(&plan, "HOMEBOY_REFRESH_SOURCE_SHA=abc123\n", &identity)
        .expect("explicitly clean identity is accepted");
}

#[test]
fn materialized_identity_rejects_explicit_dirty_state() {
    let plan = ssh_bootstrap_plan();
    let identity = serde_json::json!({
        "data": { "git_commit": "abc123", "git_dirty": true }
    });

    let error =
        verify_materialized_identity(&plan, "HOMEBOY_REFRESH_SOURCE_SHA=abc123\n", &identity)
            .expect_err("explicitly dirty identity is rejected");

    assert!(error.contains("not a clean build"));
}
