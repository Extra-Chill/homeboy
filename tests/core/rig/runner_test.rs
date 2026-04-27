//! Tests for `src/core/rig/runner.rs`.
//!
//! Two layers:
//!
//! - Report shape tests (originally authored in #1468) — verify the JSON
//!   envelope contract that CLI JSON output and scheduled jobs depend on.
//! - End-to-end tests for `run_up` / `run_check` / `run_down` / `run_status`
//!   against a minimal spec with no pipeline and no services. These exercise
//!   the bookkeeping path (state file write, report assembly) without
//!   spinning up real services. Richer integration is still smoke-tested
//!   manually per #1468's README.
//!
//! Each end-to-end test isolates `HOME` to a tempdir so the shared rig state
//! file doesn't bleed across tests or the developer's real `~/.config`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tempfile::TempDir;

use crate::rig::pipeline::PipelineOutcome;
use crate::rig::runner::{
    run_check, run_down, run_status, run_up, snapshot_state, CheckReport, RigStatusReport,
    ServiceStatusReport, UpReport,
};
use crate::rig::spec::{
    ComponentSpec, PipelineStep, RigSpec, ServiceKind, ServiceSpec, SharedPathOp, SharedPathSpec,
};

fn empty_pipeline(name: &str) -> PipelineOutcome {
    PipelineOutcome {
        name: name.to_string(),
        steps: Vec::new(),
        passed: 0,
        failed: 0,
    }
}

fn minimal_spec(id: &str) -> RigSpec {
    RigSpec {
        id: id.to_string(),
        description: format!("{} fixture", id),
        components: HashMap::new(),
        services: HashMap::new(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        pipeline: HashMap::new(),
        bench: None,
        bench_workloads: HashMap::new(),
        app_launcher: None,
    }
}

/// Serializes `HOME` env-var mutation across rig runner tests so concurrent
/// test threads can't clobber each other's state-file target. `paths::homeboy()`
/// reads `HOME` at call time, so the guard must stay alive for the full
/// duration of anything that reads/writes rig state.
fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Run `body` with `HOME` pointed at a fresh tempdir, restoring the prior
/// value when `body` returns. Held under a process-wide mutex so parallel
/// tests don't race on the shared env var.
fn with_isolated_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
    let guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("HOME").ok();
    let dir = TempDir::new().expect("create tempdir");
    std::env::set_var("HOME", dir.path());
    let result = body(&dir);
    match prior {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    drop(guard);
    result
}

// ------------------------------------------------------------------
// Report shape tests (preserved from #1468)
// ------------------------------------------------------------------

#[test]
fn test_up_report_serializes_success_flag() {
    let report = UpReport {
        rig_id: "test".to_string(),
        pipeline: empty_pipeline("up"),
        success: true,
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"rig_id\":\"test\""));
    assert!(json.contains("\"success\":true"));
}

#[test]
fn test_check_report_serializes_success_flag() {
    let report = CheckReport {
        rig_id: "test".to_string(),
        pipeline: empty_pipeline("check"),
        success: false,
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"success\":false"));
}

#[test]
fn test_status_report_empty_services_serializes() {
    let report = RigStatusReport {
        rig_id: "test".to_string(),
        description: "empty rig".to_string(),
        services: Vec::new(),
        last_up: None,
        last_check: None,
        last_check_result: None,
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"services\":[]"));
    // last_up is None -> serialized as null (not skipped, to aid tooling).
    assert!(json.contains("\"last_up\":null"));
}

#[test]
fn test_service_status_report_omits_optional_fields_when_stopped() {
    let report = ServiceStatusReport {
        id: "svc".to_string(),
        kind: "command".to_string(),
        status: "stopped".to_string(),
        pid: None,
        port: None,
        log_path: "/tmp/svc.log".to_string(),
        started_at: None,
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(!json.contains("\"pid\""));
    assert!(!json.contains("\"started_at\""));
}

#[test]
fn test_service_status_report_emits_pid_when_running() {
    let report = ServiceStatusReport {
        id: "svc".to_string(),
        kind: "http-static".to_string(),
        status: "running".to_string(),
        pid: Some(4321),
        port: Some(9724),
        log_path: "/tmp/svc.log".to_string(),
        started_at: Some("2026-04-24T13:00:00Z".to_string()),
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"pid\":4321"));
    assert!(json.contains("\"kind\":\"http-static\""));
    assert!(json.contains("\"port\":9724"));
    assert!(json.contains("\"log_path\":\"/tmp/svc.log\""));
    assert!(json.contains("\"started_at\":\"2026-04-24T13:00:00Z\""));
}

// ------------------------------------------------------------------
// End-to-end tests: each top-level runner entry point
// ------------------------------------------------------------------

#[test]
fn test_run_up() {
    with_isolated_home(|_dir| {
        let rig = minimal_spec("run-up-fixture");
        let report = run_up(&rig).expect("run_up succeeds with empty pipeline");
        assert_eq!(report.rig_id, "run-up-fixture");
        assert!(report.success, "empty pipeline should report success");
        assert_eq!(report.pipeline.passed, 0);
        assert_eq!(report.pipeline.failed, 0);
    });
}

#[test]
fn test_run_check() {
    with_isolated_home(|_dir| {
        let rig = minimal_spec("run-check-fixture");
        let report = run_check(&rig).expect("run_check succeeds with empty pipeline");
        assert_eq!(report.rig_id, "run-check-fixture");
        assert!(report.success, "empty pipeline should pass check");
        assert_eq!(report.pipeline.failed, 0);

        // Side effect: check writes last_check + last_check_result to state.
        let status = run_status(&rig).expect("run_status reads back state");
        assert_eq!(status.last_check_result.as_deref(), Some("pass"));
        assert!(status.last_check.is_some(), "last_check timestamp recorded");
    });
}

#[test]
fn test_run_down() {
    with_isolated_home(|_dir| {
        let rig = minimal_spec("run-down-fixture");
        let report = run_down(&rig).expect("run_down succeeds with no services");
        assert_eq!(report.rig_id, "run-down-fixture");
        assert!(
            report.stopped.is_empty(),
            "no services declared, nothing to stop"
        );
        assert!(
            report.pipeline.is_none(),
            "no `down` pipeline declared, so no outcome reported"
        );
        assert!(report.success, "empty teardown is trivially successful");
    });
}

#[cfg(unix)]
#[test]
fn test_run_down_cleans_state_owned_shared_paths() {
    with_isolated_home(|_dir| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let target = tmp.path().join("primary-node_modules");
        let link = tmp.path().join("worktree-node_modules");
        std::fs::create_dir(&target).expect("target dir");

        let mut pipeline = HashMap::new();
        pipeline.insert(
            "up".to_string(),
            vec![PipelineStep::SharedPath {
                step_id: None,
                depends_on: Vec::new(),
                op: SharedPathOp::Ensure,
            }],
        );
        let rig = RigSpec {
            id: "run-down-shared-path-fixture".to_string(),
            description: String::new(),
            components: HashMap::new(),
            services: HashMap::new(),
            symlinks: Vec::new(),
            shared_paths: vec![SharedPathSpec {
                link: link.to_string_lossy().into_owned(),
                target: target.to_string_lossy().into_owned(),
            }],
            pipeline,
            bench: None,
            bench_workloads: HashMap::new(),
            app_launcher: None,
        };

        let up = crate::rig::pipeline::run_pipeline(&rig, "up", true).expect("up pipeline");
        assert!(up.is_success());
        assert!(link.is_symlink());

        let down = run_down(&rig).expect("run_down");
        assert!(down.success);
        assert!(!link.exists(), "run_down removes owned shared-path symlink");
    });
}

#[test]
fn test_run_status() {
    with_isolated_home(|_dir| {
        let rig = minimal_spec("run-status-fixture");
        let status = run_status(&rig).expect("run_status succeeds with empty state");
        assert_eq!(status.rig_id, "run-status-fixture");
        assert_eq!(status.description, "run-status-fixture fixture");
        assert!(status.services.is_empty(), "no services declared");
        assert!(
            status.last_up.is_none(),
            "never brought up, so no timestamp"
        );
        assert!(status.last_check.is_none());
        assert!(status.last_check_result.is_none());

        let mut services = HashMap::new();
        services.insert(
            "assets".to_string(),
            ServiceSpec {
                kind: ServiceKind::HttpStatic,
                cwd: Some("/tmp".to_string()),
                port: Some(9724),
                command: None,
                env: HashMap::new(),
                health: None,
                discover: None,
            },
        );
        let rig = RigSpec {
            id: "run-status-service-fixture".to_string(),
            description: "service status".to_string(),
            components: HashMap::new(),
            services,
            symlinks: Vec::new(),
            shared_paths: Vec::new(),
            pipeline: HashMap::new(),
            bench: None,
            bench_workloads: HashMap::new(),
        };

        let status = run_status(&rig).expect("run_status with service");
        assert_eq!(status.services.len(), 1);
        let service = &status.services[0];
        assert_eq!(service.id, "assets");
        assert_eq!(service.kind, "http-static");
        assert_eq!(service.status, "stopped");
        assert_eq!(service.port, Some(9724));
        assert!(
            service
                .log_path
                .ends_with("run-status-service-fixture.state/logs/assets.log"),
            "unexpected log path: {}",
            service.log_path
        );
    });
}

#[test]
fn test_snapshot_state() {
    // Spec carries two components with non-existent paths. `snapshot_state`
    // should still emit one entry per component (sorted by ID) with the
    // expanded path captured and `sha`/`branch` left as `None` because
    // git won't resolve in a non-repo directory. This is the contract for
    // `homeboy bench --rig` against rigs that include components on
    // ephemeral or read-only paths.
    let mut components = HashMap::new();
    components.insert(
        "studio".to_string(),
        ComponentSpec {
            path: "/tmp/homeboy-snapshot-test-not-a-repo-z".to_string(),
            remote_url: None,
            stack: None,
            branch: None,
        },
    );
    components.insert(
        "playground".to_string(),
        ComponentSpec {
            path: "/tmp/homeboy-snapshot-test-not-a-repo-a".to_string(),
            remote_url: None,
            stack: None,
            branch: None,
        },
    );
    let rig = RigSpec {
        id: "snapshot-fixture".to_string(),
        description: String::new(),
        components,
        services: HashMap::new(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        pipeline: HashMap::new(),
        bench: None,
        bench_workloads: HashMap::new(),
        app_launcher: None,
    };

    let snapshot = snapshot_state(&rig);
    assert_eq!(snapshot.rig_id, "snapshot-fixture");
    assert!(!snapshot.captured_at.is_empty(), "timestamp populated");
    let ids: Vec<&str> = snapshot.components.keys().map(|s| s.as_str()).collect();
    assert_eq!(
        ids,
        vec!["playground", "studio"],
        "BTreeMap key order is alphabetical, independent of HashMap insertion order"
    );
    for entry in snapshot.components.values() {
        assert!(entry.sha.is_none(), "non-repo path has no HEAD SHA");
        assert!(entry.branch.is_none(), "non-repo path has no branch");
    }
}
