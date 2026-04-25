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

#[test]
fn test_parse_etime_mm_ss() {
    use crate::rig::service::parse_etime_seconds;
    // 2 minutes 30 seconds.
    assert_eq!(parse_etime_seconds("02:30"), Some(150));
    assert_eq!(parse_etime_seconds("0:01"), Some(1));
}

#[test]
fn test_parse_etime_hh_mm_ss() {
    use crate::rig::service::parse_etime_seconds;
    // 1h 02m 03s.
    assert_eq!(parse_etime_seconds("01:02:03"), Some(3_723));
}

#[test]
fn test_parse_etime_dd_hh_mm_ss() {
    use crate::rig::service::parse_etime_seconds;
    // 4 days, 9 hours, 27 minutes, 59 seconds — the format BSD `ps` emits
    // for a long-running daemon (matches what `etime` printed during dev).
    assert_eq!(parse_etime_seconds("04-09:27:59"), Some(379_679));
}

#[test]
fn test_parse_etime_rejects_garbage() {
    use crate::rig::service::parse_etime_seconds;
    assert_eq!(parse_etime_seconds(""), None);
    assert_eq!(parse_etime_seconds("not-a-time"), None);
    assert_eq!(parse_etime_seconds("01"), None);
    assert_eq!(parse_etime_seconds("a:b:c"), None);
}
