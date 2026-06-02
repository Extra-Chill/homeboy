use clap::Args;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

use homeboy::core::engine::run_dir;
use homeboy::core::observation::{
    run_owner_pid, ObservationStore, RunListFilter, RunRecord, RunStatus,
};
use homeboy::core::process::pid_is_running;

use crate::commands::runs::RunsOutput;
use crate::commands::CmdResult;

#[derive(Args, Clone, Default)]
pub struct RunsReconcileArgs {
    /// Preview orphaned running records without mutating them
    #[arg(long)]
    pub dry_run: bool,
    /// Maximum running records to inspect
    #[arg(long, default_value_t = 1000)]
    pub limit: i64,
}

#[derive(Serialize)]
pub struct RunsReconcileOutput {
    pub command: &'static str,
    pub dry_run: bool,
    pub inspected: usize,
    pub reconciled: Vec<ReconciledRunSummary>,
}

#[derive(Serialize)]
pub struct ReconciledRunSummary {
    pub id: String,
    pub kind: String,
    pub previous_status: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub owner_pid: u32,
    pub reason: String,
    pub artifact_count: usize,
}

pub fn reconcile_runs(args: RunsReconcileArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let inspected = store
        .list_runs(RunListFilter {
            status: Some(RunStatus::Running.as_str().to_string()),
            limit: Some(args.limit.clamp(1, 1000)),
            ..RunListFilter::default()
        })?
        .len();
    let reconciled =
        reconcile_orphaned_running_runs(&store, args.limit, args.dry_run, pid_is_running)?;

    Ok((
        RunsOutput::Reconcile(RunsReconcileOutput {
            command: "runs.reconcile",
            dry_run: args.dry_run,
            inspected,
            reconciled,
        }),
        0,
    ))
}

pub(super) fn reconcile_owned_stale_running_runs(
    store: &ObservationStore,
    limit: i64,
) -> homeboy::core::Result<Vec<ReconciledRunSummary>> {
    reconcile_orphaned_running_runs(store, limit, false, pid_is_running)
}

fn reconcile_orphaned_running_runs<F>(
    store: &ObservationStore,
    limit: i64,
    dry_run: bool,
    pid_is_alive: F,
) -> homeboy::core::Result<Vec<ReconciledRunSummary>>
where
    F: Fn(u32) -> bool,
{
    let running = store.list_runs(RunListFilter {
        status: Some(RunStatus::Running.as_str().to_string()),
        limit: Some(limit.clamp(1, 1000)),
        ..RunListFilter::default()
    })?;
    let mut reconciled = Vec::new();

    for run in running {
        let Some(owner_pid) = run_owner_pid(&run) else {
            continue;
        };
        if pid_is_alive(owner_pid) {
            continue;
        }

        let reason = "owner_process_not_running".to_string();
        let artifact_count = if dry_run {
            store.list_artifacts(&run.id)?.len()
        } else {
            reconcile_available_run_dir_artifacts(store, &run)?
        };
        let finished = if dry_run {
            None
        } else {
            let metadata = with_reconcile_metadata(
                &run,
                owner_pid,
                &reason,
                &reconcile_run_dir_metadata(&run),
            );
            Some(store.finish_run(&run.id, RunStatus::Stale, Some(metadata))?)
        };

        reconciled.push(ReconciledRunSummary {
            id: run.id,
            kind: run.kind,
            previous_status: run.status,
            status: RunStatus::Stale.as_str().to_string(),
            started_at: run.started_at,
            finished_at: finished.and_then(|run| run.finished_at),
            owner_pid,
            reason,
            artifact_count,
        });
    }

    Ok(reconciled)
}

pub fn running_status_note(run: &RunRecord) -> Option<String> {
    homeboy::core::observation::running_status_note(run)
}

fn with_reconcile_metadata(
    run: &RunRecord,
    owner_pid: u32,
    reason: &str,
    run_dir_metadata: &Value,
) -> Value {
    let mut metadata = run.metadata_json.clone();
    let mut marker = serde_json::json!({
        "status": RunStatus::Stale.as_str(),
        "reason": reason,
        "owner_pid": owner_pid,
        "reconciled_at": chrono::Utc::now().to_rfc3339(),
    });
    if let (Some(marker), Some(run_dir_metadata)) =
        (marker.as_object_mut(), run_dir_metadata.as_object())
    {
        for (key, value) in run_dir_metadata {
            marker.insert(key.clone(), value.clone());
        }
    }

    if let Some(object) = metadata.as_object_mut() {
        object.insert("homeboy_reconciled".to_string(), marker);
        return metadata;
    }

    serde_json::json!({
        "homeboy_reconciled": marker,
        "homeboy_original_metadata": metadata,
    })
}

fn reconcile_available_run_dir_artifacts(
    store: &ObservationStore,
    run: &RunRecord,
) -> homeboy::core::Result<usize> {
    if let Some(path) = run_dir_path(run) {
        let resource_summary = path.join(run_dir::files::RESOURCE_SUMMARY);
        if resource_summary.is_file() {
            let already_recorded = store
                .list_artifacts(&run.id)?
                .iter()
                .any(|artifact| artifact.kind == "resource_summary");
            if !already_recorded {
                let _ = store.record_artifact(&run.id, "resource_summary", &resource_summary);
            }
        }
    }

    Ok(store.list_artifacts(&run.id)?.len())
}

fn reconcile_run_dir_metadata(run: &RunRecord) -> Value {
    let Some(path) = run_dir_path(run) else {
        return Value::Null;
    };
    let extension_children = read_extension_children(&path);
    if extension_children.is_empty() {
        return Value::Null;
    }
    serde_json::json!({
        "run_dir": path.to_string_lossy().to_string(),
        "extension_children": extension_children,
    })
}

fn run_dir_path(run: &RunRecord) -> Option<PathBuf> {
    run.metadata_json
        .get("run_dir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

fn read_extension_children(run_dir_path: &Path) -> Vec<Value> {
    let dir = run_dir_path.join(run_dir::files::EXTENSION_CHILDREN_DIR);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(&content) {
            children.push(value);
        }
    }
    children.sort_by(|a, b| {
        a.get("started_at")
            .and_then(Value::as_str)
            .cmp(&b.get("started_at").and_then(Value::as_str))
            .then(
                a.get("root_pid")
                    .and_then(Value::as_u64)
                    .cmp(&b.get("root_pid").and_then(Value::as_u64)),
            )
    });
    children
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::observation::NewRunRecord;
    use homeboy::test_support::with_isolated_home;

    struct XdgGuard(Option<String>);

    impl XdgGuard {
        fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self(prior)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    fn sample_run(kind: &str, component_id: &str, rig_id: &str, metadata: Value) -> NewRunRecord {
        NewRunRecord::builder(kind)
            .component_id(component_id)
            .command(format!("homeboy {kind} {component_id}"))
            .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
            .homeboy_version("test-version")
            .git_sha(Some("abc123".to_string()))
            .rig_id(rig_id)
            .metadata(metadata)
            .build()
    }

    #[test]
    fn reconcile_marks_dead_owner_stale_and_preserves_artifacts() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({ "scenario": "fixture" }),
                ))
                .expect("run");
            let artifact_path = home.path().join("bench-results.json");
            std::fs::write(&artifact_path, b"{}").expect("artifact");
            store
                .record_artifact(&run.id, "bench_results", &artifact_path)
                .expect("record artifact");

            let reconciled =
                reconcile_orphaned_running_runs(&store, 1000, false, |_| false).expect("reconcile");
            let updated = store
                .get_run(&run.id)
                .expect("get run")
                .expect("run exists");

            assert_eq!(reconciled.len(), 1);
            assert_eq!(reconciled[0].id, run.id);
            assert_eq!(reconciled[0].previous_status, "running");
            assert_eq!(reconciled[0].status, "stale");
            assert_eq!(reconciled[0].artifact_count, 1);
            assert_eq!(updated.status, "stale");
            assert!(updated.finished_at.is_some());
            assert_eq!(updated.metadata_json["scenario"], "fixture");
            assert_eq!(
                updated.metadata_json["homeboy_reconciled"]["status"],
                "stale"
            );
            assert_eq!(store.list_artifacts(&run.id).expect("artifacts").len(), 1);
        });
    }

    #[test]
    fn reconcile_preserves_run_dir_child_metadata_when_parent_was_killed() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run_dir = home.path().join("homeboy-run-fixture");
            let children_dir = run_dir.join(run_dir::files::EXTENSION_CHILDREN_DIR);
            std::fs::create_dir_all(&children_dir).expect("children dir");
            std::fs::write(run_dir.join(run_dir::files::RESOURCE_SUMMARY), b"{}").expect("summary");
            std::fs::write(
                children_dir.join("123.json"),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "root_pid": 123,
                    "command_label": "bench-runner.sh",
                    "started_at": "2026-06-02T02:48:48.860570691+00:00",
                    "finished_at": "2026-06-02T02:48:48.966939782+00:00",
                    "duration_ms": 106,
                    "sampled_peak_rss_bytes": 2052096,
                    "sampled_peak_cpu_percent": 0,
                    "warnings": []
                }))
                .expect("serialize child"),
            )
            .expect("child summary");

            let run = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({ "run_dir": run_dir }),
                ))
                .expect("run");

            let reconciled =
                reconcile_orphaned_running_runs(&store, 1000, false, |_| false).expect("reconcile");
            let updated = store
                .get_run(&run.id)
                .expect("get run")
                .expect("run exists");

            assert_eq!(reconciled.len(), 1);
            assert_eq!(reconciled[0].artifact_count, 1);
            assert_eq!(updated.status, "stale");
            assert_eq!(
                updated.metadata_json["homeboy_reconciled"]["extension_children"][0]["root_pid"],
                123
            );
            assert_eq!(store.list_artifacts(&run.id).expect("artifacts").len(), 1);
        });
    }

    #[test]
    fn reconcile_dry_run_reports_without_mutating() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
                .expect("run");

            let reconciled =
                reconcile_orphaned_running_runs(&store, 1000, true, |_| false).expect("reconcile");
            let unchanged = store
                .get_run(&run.id)
                .expect("get run")
                .expect("run exists");

            assert_eq!(reconciled.len(), 1);
            assert!(reconciled[0].finished_at.is_none());
            assert_eq!(unchanged.status, "running");
            assert!(unchanged.finished_at.is_none());
        });
    }

    #[test]
    fn running_summary_flags_unverifiable_and_dead_owner_records() {
        let base = RunRecord {
            id: "run-1".to_string(),
            kind: "bench".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-05-01T00:00:00Z".to_string(),
            finished_at: None,
            status: "running".to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: Some("/tmp".to_string()),
            homeboy_version: Some("test".to_string()),
            git_sha: Some("abc123".to_string()),
            rig_id: Some("studio".to_string()),
            metadata_json: serde_json::json!({}),
        };

        let unverifiable = running_status_note(&base);
        assert!(unverifiable
            .as_deref()
            .expect("status note")
            .contains("no owner metadata"));

        let mut dead_owner = base;
        dead_owner.metadata_json = serde_json::json!({
            "homeboy_run_owner": { "pid": u32::MAX }
        });
        let dead_owner = running_status_note(&dead_owner);
        assert!(dead_owner
            .as_deref()
            .expect("status note")
            .contains("owner process is not running"));
    }
}
