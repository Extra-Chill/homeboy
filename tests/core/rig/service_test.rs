//! Service supervisor tests for `src/core/rig/service.rs`.
//!
//! The process lifecycle (spawn / SIGTERM / SIGKILL) is validated manually
//! in the end-to-end smoke described in #1468. Unit scope here covers the
//! pure types and status-enum ergonomics that back the runner's reporting.

use crate::rig::service::ServiceStatus;

#[test]
fn test_service_status_variants_distinguish() {
    let running = ServiceStatus::Running(42);
    let stopped = ServiceStatus::Stopped;
    let stale = ServiceStatus::Stale(42);

    assert_ne!(running, stopped);
    assert_ne!(running, stale);
    assert_ne!(stopped, stale);
}

#[test]
fn test_service_status_running_carries_pid() {
    match ServiceStatus::Running(12345) {
        ServiceStatus::Running(pid) => assert_eq!(pid, 12345),
        other => panic!("expected Running, got {:?}", other),
    }
}

#[test]
fn test_service_status_stale_carries_pid() {
    match ServiceStatus::Stale(67890) {
        ServiceStatus::Stale(pid) => assert_eq!(pid, 67890),
        other => panic!("expected Stale, got {:?}", other),
    }
}
