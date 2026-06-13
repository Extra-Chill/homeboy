//! Tests for active rig run leases.

use crate::core::error::ErrorCode;
use crate::core::rig::lease::{acquire_active_run_lease, active_run_leases};
use crate::core::rig::spec::{RigResourcesSpec, RigSpec};
use crate::core::rig::{run_up, RigRunLease};
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
        trace_workloads: Default::default(),
        trace_workload_defaults: Default::default(),
        trace_variants: Default::default(),
        trace_profiles: Default::default(),
        trace_experiments: Default::default(),
        trace_guardrails: Default::default(),
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

fn namespaced_resources(namespace_env: &str) -> RigResourcesSpec {
    RigResourcesSpec {
        exclusive: vec![format!("studio-runtime:${{env.{}}}", namespace_env)],
        paths: Vec::new(),
        ports: Vec::new(),
        process_patterns: Vec::new(),
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
fn test_acquire_active_run_lease_blocks_env_expanded_exclusive_resources() {
    with_isolated_home(|_| {
        let previous = std::env::var("RIG_LEASE_NAMESPACE").ok();
        std::env::set_var("RIG_LEASE_NAMESPACE", "bench-a");

        let studio = rig("studio", namespaced_resources("RIG_LEASE_NAMESPACE"));
        let studio_bfb = rig("studio-bfb", namespaced_resources("RIG_LEASE_NAMESPACE"));

        let lease = acquire_active_run_lease(&studio, "up")
            .expect("first lease")
            .expect("resourceful rig leases");
        let conflict =
            acquire_active_run_lease(&studio_bfb, "up").expect_err("expanded token conflicts");

        match previous {
            Some(value) => std::env::set_var("RIG_LEASE_NAMESPACE", value),
            None => std::env::remove_var("RIG_LEASE_NAMESPACE"),
        }

        assert_eq!(conflict.code, ErrorCode::RigResourceConflict);
        assert!(conflict.message.contains("studio-runtime:bench-a"));

        drop(lease);
    });
}

#[test]
fn test_acquire_active_run_lease_uses_default_namespace_for_empty_exclusive_resource_suffix() {
    with_isolated_home(|_| {
        let previous = std::env::var("RIG_LEASE_NAMESPACE").ok();
        std::env::remove_var("RIG_LEASE_NAMESPACE");

        let studio = rig("studio", namespaced_resources("RIG_LEASE_NAMESPACE"));
        let studio_bfb = rig("studio-bfb", namespaced_resources("RIG_LEASE_NAMESPACE"));

        let lease = acquire_active_run_lease(&studio, "trace")
            .expect("first lease")
            .expect("resourceful rig leases");
        let conflict = acquire_active_run_lease(&studio_bfb, "trace")
            .expect_err("default namespace token conflicts");

        match previous {
            Some(value) => std::env::set_var("RIG_LEASE_NAMESPACE", value),
            None => std::env::remove_var("RIG_LEASE_NAMESPACE"),
        }

        assert_eq!(conflict.code, ErrorCode::RigResourceConflict);
        assert!(conflict.message.contains("studio-runtime:<default>"));

        drop(lease);
    });
}

#[test]
fn test_active_run_leases_lists_live_leases_without_mutating_them() {
    with_isolated_home(|_| {
        let studio = rig("studio", resources());

        let lease = acquire_active_run_lease(&studio, "up")
            .expect("first lease")
            .expect("resourceful rig leases");
        let leases = active_run_leases().expect("list active leases");

        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].rig_id, "studio");
        assert_eq!(leases[0].command, "up");
        assert_eq!(leases[0].pid, std::process::id());

        drop(lease);
        assert!(active_run_leases()
            .expect("list active leases after drop")
            .is_empty());
    });
}

#[test]
fn test_trace_compare_lease_allows_same_process_child_trace() {
    with_isolated_home(|_| {
        let studio = rig("studio", resources());

        let compare_lease = acquire_active_run_lease(&studio, "trace compare")
            .expect("compare lease")
            .expect("resourceful rig leases");
        let child_lease = acquire_active_run_lease(&studio, "trace")
            .expect("child trace lease should be allowed under compare");

        assert!(child_lease.is_none());
        assert_eq!(active_run_leases().expect("list active leases").len(), 1);
        drop(compare_lease);
        assert!(active_run_leases()
            .expect("list active leases after drop")
            .is_empty());
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
        let lease_dir = crate::core::paths::rig_leases_dir().expect("lease dir");
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
