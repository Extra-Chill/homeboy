use clap::Args;
use serde::Serialize;
use serde_json::Value;

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
        let artifact_count = store.list_artifacts(&run.id)?.len();
        let finished = if dry_run {
            None
        } else {
            let metadata = with_reconcile_metadata(&run, owner_pid, &reason);
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

fn with_reconcile_metadata(run: &RunRecord, owner_pid: u32, reason: &str) -> Value {
    let mut metadata = run.metadata_json.clone();
    let marker = serde_json::json!({
        "status": RunStatus::Stale.as_str(),
        "reason": reason,
        "owner_pid": owner_pid,
        "reconciled_at": chrono::Utc::now().to_rfc3339(),
    });

    if let Some(object) = metadata.as_object_mut() {
        object.insert("homeboy_reconciled".to_string(), marker);
        return metadata;
    }

    serde_json::json!({
        "homeboy_reconciled": marker,
        "homeboy_original_metadata": metadata,
    })
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
