//! Tests for active rig run leases.

use crate::error::ErrorCode;
use crate::rig::lease::acquire_active_run_lease;
use crate::rig::spec::{RigResourcesSpec, RigSpec};
use crate::rig::{run_up, RigRunLease};
use crate::test_support::with_isolated_home;

fn rig(id: &str, resources: RigResourcesSpec) -> RigSpec {
    RigSpec {
        id: id.to_string(),
        description: String::new(),
        components: Default::default(),
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        resources,
        pipeline: Default::default(),
        bench: None,
        bench_workloads: Default::default(),
        bench_profiles: Default::default(),
        app_launcher: None,
    }
}

fn resources() -> RigResourcesSpec {
    RigResourcesSpec {
        exclusive: vec!["studio-runtime".to_string()],
        paths: vec!["~/Developer/studio".to_string()],
        ports: vec![9724],
        process_patterns: vec!["wordpress-server-child.mjs".to_string()],
    }
}

#[test]
fn test_acquire_active_run_lease_blocks_overlapping_resources_until_drop() {
    with_isolated_home(|_| {
        let studio = rig("studio", resources());
        let studio_bfb = rig("studio-bfb", resources());

        let lease = acquire_active_run_lease(&studio, "up")
            .expect("first lease")
            .expect("resourceful rig leases");
        let conflict =
            acquire_active_run_lease(&studio_bfb, "up").expect_err("second lease conflicts");
        assert_eq!(conflict.code, ErrorCode::RigResourceConflict);
        assert!(conflict.message.contains("studio-runtime"));
        assert!(conflict.message.contains("studio"));

        drop(lease);
        assert!(acquire_active_run_lease(&studio_bfb, "up")
            .expect("lease after drop")
            .is_some());
    });
}

#[test]
fn test_acquire_active_run_lease_prunes_stale_pid() {
    with_isolated_home(|_| {
        let stale = RigRunLease {
            rig_id: "studio".to_string(),
            command: "up".to_string(),
            pid: u32::MAX,
            started_at: "2026-04-27T00:00:00Z".to_string(),
            resources: resources(),
        };
        let lease_dir = crate::paths::rig_leases_dir().expect("lease dir");
        std::fs::create_dir_all(&lease_dir).expect("create lease dir");
        std::fs::write(
            lease_dir.join("studio.json"),
            serde_json::to_string_pretty(&stale).expect("serialize stale lease"),
        )
        .expect("write stale lease");

        let studio_bfb = rig("studio-bfb", resources());
        assert!(acquire_active_run_lease(&studio_bfb, "up")
            .expect("stale pid ignored")
            .is_some());
    });
}

#[test]
fn test_run_up_acquires_active_run_lease() {
    with_isolated_home(|_| {
        let studio = rig("studio", resources());
        let studio_bfb = rig("studio-bfb", resources());
        let _lease = acquire_active_run_lease(&studio, "up")
            .expect("first lease")
            .expect("resourceful rig leases");

        let conflict =
            run_up(&studio_bfb).expect_err("run_up should acquire lease before pipeline");
        assert_eq!(conflict.code, ErrorCode::RigResourceConflict);
    });
}
