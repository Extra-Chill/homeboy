#![cfg(test)]

mod part_a;
mod part_b;
mod part_c;
mod part_d;

use std::collections::HashMap;

use serde_json::json;

use super::*;
use uuid::Uuid;

/// The durable store in its legacy document shape, read from the SQL store.
pub(super) fn durable_snapshot(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(
        &JobStore::export_legacy_snapshot_at_path(path).expect("export durable store"),
    )
    .expect("exported snapshot is JSON")
}

/// Replace the durable store with a pre-SQLite `jobs.json` document, as an
/// older daemon would have left it. The next open imports it.
pub(super) fn install_legacy_store(path: &std::path::Path, snapshot: &serde_json::Value) {
    let _ = std::fs::remove_file(super::persistence::tombstone_path(path));
    std::fs::write(
        path,
        serde_json::to_vec_pretty(snapshot).expect("encode legacy store"),
    )
    .expect("write legacy store");
}

pub(super) fn record_test_local_child(store: &JobStore, job_id: Uuid, pid: u32) {
    store
        .reserve_local_child_at_with_runner_capacity(job_id, crate::api_jobs::timestamp_ms(), None)
        .map(|_| ())
        .expect("reserve local child");
    store
        .start_with_reserved_child_identity(
            job_id,
            pid,
            None,
            if cfg!(target_os = "linux") {
                super::store::LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks { ticks: 1 }
            } else {
                super::store::LocalChildStartDiscriminator::Unsupported {
                    evidence: "test platform has no Linux start-time identity".to_string(),
                }
            },
        )
        .expect("record child identity");
}

pub(super) fn remote_runner_request(
    runner_id: &str,
    project_id: Option<&str>,
) -> RemoteRunnerJobRequest {
    RemoteRunnerJobRequest {
        runner_id: runner_id.to_string(),
        project_id: project_id.map(str::to_string),
        operation: "runner.exec".to_string(),
        command: vec!["homeboy".to_string(), "test".to_string()],
        cwd: Some("/srv/extrachill".to_string()),
        env: HashMap::new(),
        extension_env_providers: Vec::new(),
        secret_env_names: Vec::new(),
        secret_env_plan: Default::default(),
        env_materialization: None,
        capture_patch: true,
        source_snapshot: Some(crate::source_snapshot::existing_remote(
            runner_id,
            "/srv/extrachill",
            Some("/srv"),
        )),
        path_materialization_plan: None,
        require_paths: Vec::new(),
        lab_runner_workload: None,
        lifecycle: None,
        workspace_claim_binding: None,
        workspace_owner_lease: None,
        metadata: Some(json!({ "submitted_by": "controller" })),
    }
}
