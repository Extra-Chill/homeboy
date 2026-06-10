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

use crate::core::rig::pipeline::PipelineOutcome;
use crate::core::rig::runner::{
    head_sha_and_branch, run_check, run_check_groups, run_down, run_repair, run_status, run_up,
    snapshot_state, CheckReport, RigStatusReport, ServiceStatusReport, SymlinkStatusState,
    UpReport,
};
use crate::core::rig::spec::{
    ComponentSpec, PipelineStep, RigResourcesSpec, RigSpec, ServiceKind, ServiceSpec, SharedPathOp,
    SharedPathSpec, SymlinkSpec,
};
use crate::core::rig::state::RigState;
use crate::test_support::with_isolated_home;

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
        resources: Default::default(),
        pipeline: HashMap::new(),
        bench: None,
        bench_workloads: HashMap::new(),
        trace_workloads: HashMap::new(),
        trace_workload_defaults: HashMap::new(),
        trace_variants: HashMap::new(),
        trace_profiles: HashMap::new(),
        trace_experiments: HashMap::new(),
        trace_guardrails: Vec::new(),
        bench_profiles: HashMap::new(),
        app_launcher: None,
    }
}

fn symlink_spec(id: &str, link: String, target: String) -> RigSpec {
    RigSpec {
        symlinks: vec![SymlinkSpec { link, target }],
        ..minimal_spec(id)
    }
}

#[cfg(unix)]
fn unix_symlink(target: &std::path::Path, link: &std::path::Path) {
    std::os::unix::fs::symlink(target, link).expect("create symlink");
}

// ------------------------------------------------------------------
// Report shape tests (preserved from #1468)
// ------------------------------------------------------------------

#[test]
fn test_up_report_serializes_success_flag() {
    let report = UpReport {
        rig_id: "test".to_string(),
        run_id: None,
        pipeline: empty_pipeline("up"),
        success: true,
        artifact_index: None,
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"rig_id\":\"test\""));
    assert!(json.contains("\"success\":true"));
}

#[test]
fn test_check_report_serializes_success_flag() {
    let report = CheckReport {
        rig_id: "test".to_string(),
        run_id: None,
        pipeline: empty_pipeline("check"),
        success: false,
        artifact_index: None,
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
        symlinks: Vec::new(),
        last_up: None,
        last_check: None,
        last_check_result: None,
        materialized: None,
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
        let mut rig = minimal_spec("run-up-fixture");
        rig.resources = RigResourcesSpec {
            exclusive: vec!["run-up-exclusive".to_string()],
            paths: vec!["/tmp/run-up-path".to_string()],
            ports: vec![9981],
            process_patterns: vec!["run-up-process".to_string()],
        };
        let report = run_up(&rig).expect("run_up succeeds with empty pipeline");
        assert_eq!(report.rig_id, "run-up-fixture");
        assert!(report.success, "empty pipeline should report success");
        assert_eq!(report.pipeline.passed, 0);
        assert_eq!(report.pipeline.failed, 0);

        let state = RigState::load(&rig.id).expect("state loads");
        let materialized = state.materialized.expect("materialized ownership");
        assert_eq!(materialized.rig_id, "run-up-fixture");
        assert_eq!(materialized.resources.exclusive, vec!["run-up-exclusive"]);
        assert_eq!(materialized.resources.ports, vec![9981]);
        assert!(
            materialized.components.is_empty(),
            "minimal spec has no component snapshot entries"
        );
    });
}

#[test]
fn test_failed_run_up_does_not_write_materialized_ownership() {
    with_isolated_home(|_dir| {
        let mut rig = minimal_spec("run-up-failure-fixture");
        let mut pipeline = HashMap::new();
        pipeline.insert(
            "up".to_string(),
            vec![PipelineStep::Command {
                step_id: None,
                depends_on: Vec::new(),
                cmd: "false".to_string(),
                cwd: None,
                env: HashMap::new(),
                label: Some("intentional failure".to_string()),
            }],
        );
        rig.pipeline = pipeline;

        let report = run_up(&rig).expect("run_up returns a failed report");
        assert!(!report.success, "failing step should fail up");

        let state = RigState::load(&rig.id).expect("state loads");
        assert!(
            state.materialized.is_none(),
            "failed up must not persist materialized ownership"
        );
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
fn test_run_check_groups_runs_only_matching_check_steps() {
    with_isolated_home(|_dir| {
        let rig: RigSpec = serde_json::from_str(
            r#"{
                "id": "scoped-check-fixture",
                "pipeline": {
                    "check": [
                        {
                            "kind": "check",
                            "label": "desktop app exists",
                            "groups": ["desktop-app"],
                            "command": "true"
                        },
                        {
                            "kind": "check",
                            "label": "unrelated cli symlink",
                            "groups": ["cli-dev-copy"],
                            "command": "false"
                        },
                        {
                            "kind": "symlink",
                            "op": "verify"
                        }
                    ]
                }
            }"#,
        )
        .expect("parse rig");

        let report =
            run_check_groups(&rig, &["desktop-app".to_string()]).expect("scoped check runs");

        assert!(report.success, "unselected failing checks are skipped");
        assert_eq!(report.pipeline.steps.len(), 1);
        assert_eq!(report.pipeline.steps[0].label, "desktop app exists");
        assert_eq!(report.pipeline.passed, 1);
        assert_eq!(report.pipeline.failed, 0);

        let full = run_check(&rig).expect("full check returns failed report");
        assert!(!full.success, "full rig check remains strict");
        assert!(full.pipeline.failed >= 1);
    });
}

#[test]
fn test_run_down() {
    with_isolated_home(|_dir| {
        let rig = minimal_spec("run-down-fixture");
        run_up(&rig).expect("seed materialized ownership");
        assert!(
            RigState::load(&rig.id)
                .expect("state loads")
                .materialized
                .is_some(),
            "precondition: up writes ownership"
        );

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
        assert!(
            RigState::load(&rig.id)
                .expect("state loads")
                .materialized
                .is_none(),
            "down clears materialized ownership"
        );
    });
}

#[cfg(unix)]
#[test]
fn test_run_repair_repairs_drifted_symlink() {
    with_isolated_home(|_dir| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let old_target = tmp.path().join("old-target");
        let expected_target = tmp.path().join("expected-target");
        let link = tmp.path().join("tool");
        std::fs::write(&old_target, "old").expect("old target");
        std::fs::write(&expected_target, "expected").expect("expected target");
        unix_symlink(&old_target, &link);

        let rig = symlink_spec(
            "repair-drifted-symlink-fixture",
            link.to_string_lossy().into_owned(),
            expected_target.to_string_lossy().into_owned(),
        );

        let report = run_repair(&rig).expect("repair succeeds");
        assert!(report.success);
        assert_eq!(report.repaired, 1);
        assert_eq!(report.unchanged, 0);
        assert_eq!(report.blocked, 0);
        assert_eq!(report.resources[0].status, "repaired");
        assert_eq!(
            report.resources[0].previous_target.as_deref(),
            Some(old_target.to_string_lossy().as_ref())
        );
        assert_eq!(
            std::fs::read_link(&link).expect("read link"),
            expected_target
        );
    });
}

#[cfg(unix)]
#[test]
fn test_run_repair_creates_missing_symlink() {
    with_isolated_home(|_dir| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let expected_target = tmp.path().join("expected-target");
        let link = tmp.path().join("nested").join("tool");
        std::fs::write(&expected_target, "expected").expect("expected target");

        let rig = symlink_spec(
            "repair-missing-symlink-fixture",
            link.to_string_lossy().into_owned(),
            expected_target.to_string_lossy().into_owned(),
        );

        let report = run_repair(&rig).expect("repair succeeds");
        assert!(report.success);
        assert_eq!(report.repaired, 1);
        assert_eq!(report.resources[0].status, "repaired");
        assert_eq!(report.resources[0].previous_target, None);
        assert_eq!(
            std::fs::read_link(&link).expect("read link"),
            expected_target
        );
    });
}

#[cfg(unix)]
#[test]
fn test_run_repair_noops_ok_symlink() {
    with_isolated_home(|_dir| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let expected_target = tmp.path().join("expected-target");
        let link = tmp.path().join("tool");
        std::fs::write(&expected_target, "expected").expect("expected target");
        unix_symlink(&expected_target, &link);

        let rig = symlink_spec(
            "repair-ok-symlink-fixture",
            link.to_string_lossy().into_owned(),
            expected_target.to_string_lossy().into_owned(),
        );

        let report = run_repair(&rig).expect("repair succeeds");
        assert!(report.success);
        assert_eq!(report.repaired, 0);
        assert_eq!(report.unchanged, 1);
        assert_eq!(report.blocked, 0);
        assert_eq!(report.resources[0].status, "unchanged");
        assert_eq!(
            std::fs::read_link(&link).expect("read link"),
            expected_target
        );
    });
}

#[cfg(unix)]
#[test]
fn test_run_repair_blocks_non_symlink() {
    with_isolated_home(|_dir| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let expected_target = tmp.path().join("expected-target");
        let link = tmp.path().join("tool");
        std::fs::write(&expected_target, "expected").expect("expected target");
        std::fs::write(&link, "real file").expect("real file at link path");

        let rig = symlink_spec(
            "repair-blocked-symlink-fixture",
            link.to_string_lossy().into_owned(),
            expected_target.to_string_lossy().into_owned(),
        );

        let report = run_repair(&rig).expect("repair reports blocked resource");
        assert!(!report.success);
        assert_eq!(report.repaired, 0);
        assert_eq!(report.unchanged, 0);
        assert_eq!(report.blocked, 1);
        assert_eq!(report.resources[0].status, "blocked");
        assert!(report.resources[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("not a symlink"));
        assert_eq!(
            std::fs::read_to_string(&link).expect("read file"),
            "real file"
        );
    });
}

#[cfg(unix)]
#[test]
fn test_run_repair_does_not_run_pipeline_commands() {
    with_isolated_home(|_dir| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let expected_target = tmp.path().join("expected-target");
        let link = tmp.path().join("tool");
        let command_marker = tmp.path().join("command-ran");
        std::fs::write(&expected_target, "expected").expect("expected target");

        let mut rig = symlink_spec(
            "repair-skip-pipeline-fixture",
            link.to_string_lossy().into_owned(),
            expected_target.to_string_lossy().into_owned(),
        );
        rig.pipeline.insert(
            "up".to_string(),
            vec![PipelineStep::Command {
                step_id: None,
                depends_on: Vec::new(),
                cmd: format!("printf ran > {}", command_marker.to_string_lossy()),
                cwd: None,
                env: HashMap::new(),
                label: Some("must-not-run".to_string()),
            }],
        );

        let report = run_repair(&rig).expect("repair succeeds");
        assert!(report.success);
        assert_eq!(report.repaired, 1);
        assert!(link.is_symlink(), "repair still handled declared symlink");
        assert!(
            !command_marker.exists(),
            "repair must not run heavy or arbitrary pipeline commands"
        );
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
            resources: Default::default(),
            pipeline,
            bench: None,
            bench_workloads: HashMap::new(),
            trace_workloads: HashMap::new(),
            trace_workload_defaults: HashMap::new(),
            trace_variants: HashMap::new(),
            trace_profiles: HashMap::new(),
            trace_experiments: HashMap::new(),
            trace_guardrails: Vec::new(),
            bench_profiles: HashMap::new(),
            app_launcher: None,
        };

        let up = crate::core::rig::pipeline::run_pipeline(&rig, "up", true).expect("up pipeline");
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
        assert!(status.materialized.is_none());

        run_up(&rig).expect("seed materialized ownership");
        let status = run_status(&rig).expect("run_status reports materialized state");
        let materialized = status.materialized.expect("materialized ownership");
        assert_eq!(materialized.rig_id, "run-status-fixture");
        assert!(!materialized.materialized_at.is_empty());

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
            resources: Default::default(),
            pipeline: HashMap::new(),
            bench: None,
            app_launcher: None,
            bench_workloads: HashMap::new(),
            trace_workloads: HashMap::new(),
            trace_workload_defaults: HashMap::new(),
            trace_variants: HashMap::new(),
            trace_profiles: HashMap::new(),
            trace_experiments: HashMap::new(),
            trace_guardrails: Vec::new(),
            bench_profiles: HashMap::new(),
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

#[cfg(unix)]
#[test]
fn test_run_status_reports_declared_symlink_states() {
    with_isolated_home(|_dir| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let expected = tmp.path().join("expected-target");
        let other = tmp.path().join("other-target");
        let ok = tmp.path().join("ok-link");
        let missing = tmp.path().join("missing-link");
        let drifted = tmp.path().join("drifted-link");
        let blocked = tmp.path().join("blocked-link");

        std::fs::create_dir(&expected).expect("expected target");
        std::fs::create_dir(&other).expect("other target");
        std::os::unix::fs::symlink(&expected, &ok).expect("ok symlink");
        std::os::unix::fs::symlink(&other, &drifted).expect("drifted symlink");
        std::fs::write(&blocked, "not a symlink").expect("blocked file");

        let mut rig = minimal_spec("run-status-symlink-fixture");
        rig.symlinks = vec![
            SymlinkSpec {
                link: ok.to_string_lossy().into_owned(),
                target: expected.to_string_lossy().into_owned(),
            },
            SymlinkSpec {
                link: missing.to_string_lossy().into_owned(),
                target: expected.to_string_lossy().into_owned(),
            },
            SymlinkSpec {
                link: drifted.to_string_lossy().into_owned(),
                target: expected.to_string_lossy().into_owned(),
            },
            SymlinkSpec {
                link: blocked.to_string_lossy().into_owned(),
                target: expected.to_string_lossy().into_owned(),
            },
        ];

        let status = run_status(&rig).expect("run_status with symlinks");
        assert_eq!(status.symlinks.len(), 4);

        let by_link = status
            .symlinks
            .iter()
            .map(|entry| (entry.link.as_str(), entry))
            .collect::<HashMap<_, _>>();

        let ok_report = by_link
            .get(ok.to_string_lossy().as_ref())
            .expect("ok report");
        assert_eq!(ok_report.state, SymlinkStatusState::Ok);
        assert_eq!(
            ok_report.actual_target.as_deref(),
            Some(expected.to_string_lossy().as_ref())
        );
        assert_eq!(
            ok_report.expected_target,
            expected.to_string_lossy().as_ref()
        );

        let missing_report = by_link
            .get(missing.to_string_lossy().as_ref())
            .expect("missing report");
        assert_eq!(missing_report.state, SymlinkStatusState::Missing);
        assert!(missing_report.actual_target.is_none());

        let drifted_report = by_link
            .get(drifted.to_string_lossy().as_ref())
            .expect("drifted report");
        assert_eq!(drifted_report.state, SymlinkStatusState::Drifted);
        assert_eq!(
            drifted_report.actual_target.as_deref(),
            Some(other.to_string_lossy().as_ref())
        );

        let blocked_report = by_link
            .get(blocked.to_string_lossy().as_ref())
            .expect("blocked report");
        assert_eq!(
            blocked_report.state,
            SymlinkStatusState::BlockedByNonSymlink
        );
        assert!(blocked_report.actual_target.is_none());

        let json = serde_json::to_string(&status).expect("serialize status");
        assert!(json.contains("\"symlinks\""));
        assert!(json.contains("\"state\":\"ok\""));
        assert!(json.contains("\"state\":\"missing\""));
        assert!(json.contains("\"state\":\"drifted\""));
        assert!(json.contains("\"state\":\"blocked_by_non_symlink\""));
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
            checkout_root: None,
            remote_url: None,
            triage_remote_url: None,
            stack: None,
            branch: None,
            r#ref: None,
            extensions: None,
        },
    );
    components.insert(
        "playground".to_string(),
        ComponentSpec {
            path: "/tmp/homeboy-snapshot-test-not-a-repo-a".to_string(),
            checkout_root: None,
            remote_url: None,
            triage_remote_url: None,
            stack: None,
            branch: None,
            r#ref: None,
            extensions: None,
        },
    );
    let rig = RigSpec {
        id: "snapshot-fixture".to_string(),
        description: String::new(),
        components,
        services: HashMap::new(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        resources: Default::default(),
        pipeline: HashMap::new(),
        bench: None,
        bench_workloads: HashMap::new(),
        trace_workloads: HashMap::new(),
        trace_workload_defaults: HashMap::new(),
        trace_variants: HashMap::new(),
        trace_profiles: HashMap::new(),
        trace_experiments: HashMap::new(),
        trace_guardrails: Vec::new(),
        bench_profiles: HashMap::new(),
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

#[test]
fn test_head_sha_and_branch_returns_none_for_non_repo_path() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let (sha, branch) = head_sha_and_branch(&temp.path().to_string_lossy());
    assert!(sha.is_none(), "non-repo path has no HEAD SHA");
    assert!(branch.is_none(), "non-repo path has no branch");
}

#[test]
fn test_head_sha_and_branch_returns_sha_and_branch_for_git_repo() {
    use std::process::Command;

    let temp = tempfile::TempDir::new().expect("temp dir");
    let path = temp.path();

    // Initialize a minimal git repo with a deterministic commit so the
    // helper has something to read. Skip the test gracefully when git is
    // unavailable so the suite stays portable.
    let init = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg("--initial-branch=fixture-main")
        .current_dir(path)
        .status();
    let Ok(status) = init else {
        eprintln!("git not available; skipping head_sha_and_branch repo test");
        return;
    };
    if !status.success() {
        eprintln!("git init failed; skipping head_sha_and_branch repo test");
        return;
    }

    for (key, value) in [
        ("user.email", "fixture@example.test"),
        ("user.name", "Fixture"),
        ("commit.gpgsign", "false"),
    ] {
        let _ = Command::new("git")
            .args(["config", key, value])
            .current_dir(path)
            .status();
    }

    std::fs::write(path.join("README"), b"fixture\n").expect("write readme");
    let _ = Command::new("git")
        .args(["add", "README"])
        .current_dir(path)
        .status();
    let commit = Command::new("git")
        .args(["commit", "--quiet", "-m", "fixture"])
        .current_dir(path)
        .status();
    if !commit.map(|s| s.success()).unwrap_or(false) {
        eprintln!("git commit failed; skipping head_sha_and_branch repo test");
        return;
    }

    let (sha, branch) = head_sha_and_branch(&path.to_string_lossy());
    let sha = sha.expect("HEAD SHA on a fresh git repo");
    assert_eq!(sha.len(), 40, "HEAD SHA is full 40-char hex: {sha}");
    assert!(
        sha.chars().all(|c| c.is_ascii_hexdigit()),
        "HEAD SHA is hex: {sha}"
    );
    assert_eq!(branch.as_deref(), Some("fixture-main"));
}
